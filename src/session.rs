//! A program running in a PTY of its own, or a task: Claude Code run
//! without a terminal.

use crate::agent_rules;
use crate::agent_screen::{self, Looks, ScreenWatch};
use crate::agents;
use crate::claude_title;
use crate::codex::Rollouts;
use crate::config::Config;
use crate::distill::Material;
use crate::events::{ToolUse, now_ms};
use crate::front;
use crate::git::Checkout;
use crate::handover::{self, Got};
use crate::keys;
use crate::model;
use crate::notify::{self, Notice};
use crate::output_ring::OutputRing;
use crate::printable;
use crate::protocol::{
    Activity, AgentEvent, AgentReport, Answer, Asking, Conversation, Front, InProgress, Metadata,
    Reporter, ScreenExplained, SessionInfo, State, TaskBrief, TaskInfo, TaskOutcome, TaskRecord,
    TaskResult, TaskSpec, TaskState, TaskView,
};
use crate::report;
use crate::spending::Spending;
use crate::state::SavedSession;
use crate::task::{self, Task};
use crate::tasks;
use crate::vt;
use anyhow::{Context, Result, ensure};
use portable_pty::{CommandBuilder, ExitStatus, MasterPty, native_pty_system};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long a stopped session gets to exit after its hang-up before it's
/// killed outright.
pub const STOP_GRACE: Duration = Duration::from_secs(2);

/// Chunks of output a viewer may fall behind by before it's dropped.
const VIEWER_BACKLOG: usize = 256;

/// The size a session's screen has, rows by columns, until a viewer gives
/// it another, as herdr does: roomy enough that an agent nobody is looking
/// at yet lays its output out as it would on a real screen, not cramped
/// into a terminal's old 80 by 24. Once seen, a session keeps the size its
/// last viewer gave it.
pub const UNSEEN_SIZE: (u16, u16) = (40, 120);

/// How often what's in front in a terminal is looked at again even when
/// its job hasn't changed: a program can replace itself with another, the
/// way `exec` does, and keep its place in front.
const FRONT_RECHECK: Duration = Duration::from_secs(2);

pub struct Session {
    pub name: String,
    /// Stays the same when the session is renamed. The program learns it
    /// from its environment, which can't change once it runs, and its hooks
    /// report with it.
    pub id: String,
    /// As it was asked for, which is how `ls` shows it.
    command: Vec<String>,
    cwd: PathBuf,
    /// The environment its program started with, which the distiller runs
    /// Claude with too. Never written down: it can hold secrets.
    env: BTreeMap<String, String>,
    pid: Option<u32>,
    state: Arc<Mutex<State>>,
    /// `None` until an agent reports what it's doing; most programs never
    /// do.
    activity: Option<Activity>,
    /// When the session last changed: it started, its activity changed, or
    /// its program ended. Shared with the thread that waits for that end.
    changed: Arc<Mutex<SystemTime>>,
    /// What the screen has been saying the agent is doing.
    screen_watch: ScreenWatch,
    /// What's in front in the terminal, once it's been looked at.
    front: Option<Front>,
    /// The job that was in front when it was last looked at, by its process
    /// group, and when: a new job in front is looked at straight away.
    front_group: Option<i32>,
    front_checked: Instant,
    /// The git worktree `cwd` is in, if it's in one.
    checkout: Option<Checkout>,
    /// The agent's conversation, once its hooks have named it, or it has
    /// turned up in Codex's rollouts.
    conversation: Option<Conversation>,
    /// Where to find a Codex session's conversation, until it's found.
    rollouts: Option<Rollouts>,
    /// What the user was last told about the session, and what waits to
    /// be told.
    telling: notify::Telling,
    /// The program rang the terminal's bell while nobody was watching, and
    /// nobody has looked since. Not handed over: a new crystal starts the
    /// marks afresh.
    bell: bool,
    /// How many times the program asked its terminal to copy something
    /// while nobody was watching, which nobody put on the clipboard. Not
    /// handed over either.
    unseen_copies: u32,
    /// `Some` for a task, whose screen shows what Claude does in its runs
    /// rather than a program in a PTY.
    task: Option<Task>,
    /// What the agent was asked to do, for a session started with
    /// something to do: a task, in a terminal or in the background.
    goal: Option<TaskInfo>,
    /// The pull request and the issue the session is about, apart from any
    /// task it has: its agent is told of them each time it starts, and they
    /// go with it through restarts and handovers.
    about: TaskBrief,
    /// Whether its agent has been reminded that its task is still open,
    /// which it is once.
    reminded: bool,
    /// The agent that says what it's doing itself, with `crystal report`,
    /// while it holds the session: its reports are the session's status,
    /// and crystal reads neither the screen nor hooks for it.
    reporter: Option<Reporter>,
    /// The job that was in front in the terminal as that agent took the
    /// session over: the agent's own. A shell in front with another job is
    /// the agent gone.
    reporter_job: Option<i32>,
    /// Whether crystal named the session after its program, and nothing
    /// has named it since: its first prompt can, then.
    named_after_program: bool,
    /// Whether the user or a script gave it its name, as it started or with
    /// a rename: a rename in Claude Code leaves it.
    name_given: bool,
    /// The name Claude Code gives its agent's conversation, to keep in step
    /// with the session's.
    title: claude_title::Watch,
    /// The agent whose conversation `conversation` is, by its program,
    /// when that isn't the session's own program: one typed into its
    /// shell, whose hooks `crystal integration` installed. A restart types
    /// the command that resumes it into the shell again.
    typed_agent: Option<String>,
    /// How many subagents its agent has running, as its hooks say.
    subagents: u32,
    /// The model its agent runs on, as its command, its hooks and its
    /// transcript say.
    model: model::Watch,
    /// What `crystal report --line` and `--model` put on its row.
    shown: report::Shown,
    /// crystal stopped it after its agent sat idle: see [`Session::idle_for`].
    stopped_idle: bool,
    /// crystal stopped it to start it again in another worktree: see
    /// [`Session::stop_to_move`].
    stopped_to_move: bool,
    /// Tasks that closed of themselves, like a background task whose run
    /// ended, for the daemon to write down.
    closed: Vec<TaskRecord>,
    /// What has happened to it since the daemon last asked, for it to tell.
    changes: Vec<Change>,
    /// For a session yet to start again after a restart, or that couldn't:
    /// what it starts from. It has no program, and is written down as it
    /// was, to start again with the next restart if it isn't before.
    start_from: Option<SavedSession>,
    term: Arc<Term>,
}

/// Something that happened to a session, kept in order until the daemon
/// takes it to tell.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// Its agent went from doing one thing to another.
    Activity {
        from: Option<Activity>,
        to: Option<Activity>,
    },
    /// A task's run started: its prompt, or a follow-up.
    RunStarted { prompt: String },
    /// That run ended, with what it came to.
    RunEnded(TaskResult),
    /// A background task's Claude asks the user for a permission.
    Asking(Asking),
    /// A background task's Claude used a tool.
    ToolUsed(ToolUse),
    /// A task its run had closed opened again, with a follow-up.
    Reopened,
    /// Its agent's turn ended with its task still open: the task waits on
    /// the user.
    TaskWaiting,
    /// An agent took the session's status over with `crystal report`.
    Claimed,
    /// The agent called this let go of it.
    Released { agent: String },
    /// Its program rang the terminal's bell while nobody was watching.
    Bell,
    /// Its program asked its terminal to copy something while nobody was
    /// watching, which nobody put on the clipboard.
    UnseenCopy,
}

/// A session as one daemon hands it to the next, in a handover: all it
/// takes to carry it on, and its terminal by the number of the descriptor
/// the next daemon inherits.
#[derive(Serialize, Deserialize)]
pub struct Handed {
    name: String,
    id: String,
    command: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    pid: Option<u32>,
    state: State,
    activity: Option<Activity>,
    changed: SystemTime,
    looks: Looks,
    front: Option<Front>,
    conversation: Option<Conversation>,
    rollouts: Option<Rollouts>,
    told: Option<Activity>,
    goal: Option<TaskInfo>,
    /// Handed over by crystals since this was.
    #[serde(default)]
    about: TaskBrief,
    reminded: bool,
    reporter: Option<Reporter>,
    reporter_job: Option<i32>,
    named_after_program: bool,
    /// Handed over by crystals since these were, and left out by those
    /// before them, which a crystal reads as none.
    #[serde(default)]
    name_given: bool,
    #[serde(default)]
    title: claude_title::Watch,
    #[serde(default)]
    typed_agent: Option<String>,
    #[serde(default)]
    subagents: u32,
    #[serde(default)]
    model: model::Watch,
    #[serde(default)]
    shown: report::Shown,
    #[serde(default)]
    stopped_idle: bool,
    /// For a session yet to start again after a restart, or that couldn't:
    /// what it starts from. Its `state` says it has ended, which is how a
    /// crystal from before these reads it.
    #[serde(default)]
    start_from: Option<StartFrom>,
    screen: vt::Saved,
    /// There will be no more output.
    ended: bool,
    /// The terminal's master side, while its program may still write to
    /// it.
    pty: Option<RawFd>,
    task: Option<task::Handed>,
}

/// What a session yet to start again starts from, as it's handed over,
/// and why it couldn't, once it has tried.
#[derive(Serialize, Deserialize)]
struct StartFrom {
    saved: SavedSession,
    failed: Option<String>,
}

impl Handed {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The processes it has running, which the next daemon has to reap:
    /// its program, or a task's `claude`s.
    pub fn processes(&self) -> Vec<u32> {
        match &self.task {
            Some(task) => task.processes(),
            None => self
                .pid
                .filter(|_| self.state == State::Running)
                .into_iter()
                .collect(),
        }
    }

    /// What it takes to start the session again, as after any restart,
    /// when it couldn't be carried on: `None` once its program had ended.
    pub fn saved(&self) -> Option<SavedSession> {
        if let Some(start_from) = &self.start_from {
            return Some(start_from.saved.clone());
        }
        if self.state != State::Running {
            return None;
        }
        let conversation = match &self.task {
            Some(task) => task.conversation().map(|id| Conversation {
                id,
                transcript: None,
                prompted: false,
            }),
            None => self.conversation.clone(),
        };
        let (conversation, resume) = restart_with(
            conversation,
            self.reporter.as_ref(),
            self.typed_agent.as_deref(),
            self.front.as_ref(),
        );
        Some(SavedSession {
            name: self.name.clone(),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            conversation,
            task: self.task.as_ref().map(|task| task.spec().clone()),
            goal: self.goal.clone(),
            resume,
            about: self.about.clone(),
            name_given: self.name_given,
            moved: None,
        })
    }
}

/// What a restart picks a session's agent up again with: the conversation,
/// for an agent crystal started itself, and the command that resumes it,
/// typed into the session's shell, or run in place of its command. An
/// agent that reports for itself said that command; one typed into the
/// shell by hand is resumed with its own command for its conversation,
/// while it's still in front, which a restart types in again; one that
/// has left the front is gone, and stays gone.
fn restart_with(
    conversation: Option<Conversation>,
    reporter: Option<&Reporter>,
    typed_agent: Option<&str>,
    front: Option<&Front>,
) -> (Option<Conversation>, Option<Vec<String>>) {
    if let Some(resume) = reporter.and_then(|reporter| reporter.resume.clone()) {
        return (conversation, Some(resume));
    }
    let Some(agent) = typed_agent else {
        return (conversation, None);
    };
    let in_front = matches!(front, Some(Front::Agent { program, .. }) if program == agent);
    let resume = conversation
        .filter(|_| in_front)
        .filter(Conversation::can_resume)
        .and_then(|conversation| agents::resume_typed(agent, &conversation.id));
    (None, resume)
}

impl Session {
    /// Starts `argv` in a PTY of its own. `command` is what was asked for;
    /// `argv` may add to it, like the flags that make an agent report what
    /// it's doing. Given `before`, the screen a terminal of the session's
    /// showed before a cold restart, its screen shows that above what the
    /// program writes (see [`vt::Screen::after_restart`]).
    pub fn spawn(
        id: String,
        name: String,
        command: Vec<String>,
        argv: &[String],
        cwd: PathBuf,
        env: &BTreeMap<String, String>,
        before: Option<&vt::Saved>,
    ) -> Result<Session> {
        let (rows, cols) = UNSEEN_SIZE;
        let pty = native_pty_system().openpty(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut builder = CommandBuilder::new(&argv[0]);
        builder.args(&argv[1..]);
        builder.cwd(&cwd);
        builder.env_clear();
        for (key, value) in env {
            builder.env(key, value);
        }
        let child = pty.slave.spawn_command(builder)?;
        // Only the child may hold the terminal's other end, so that its exit
        // ends the output.
        drop(pty.slave);
        let pid = child
            .process_id()
            .context("the program started without a pid")?;
        // Waited for by its pid, the same way a daemon it's handed over to
        // waits for it.
        drop(child);

        let pty = Some(Pty::of(pty.master)?);
        let term = match before {
            // What's on the screen isn't in the ring.
            Some(before) => {
                let vt = vt::Screen::after_restart(before, rows, cols);
                Term::with_screen(pty, vt, false)
            }
            None => Term::new(pty),
        };
        let term = Arc::new(term);
        term.start_pumping();
        let state = Arc::new(Mutex::new(State::Running));
        let changed = Arc::new(Mutex::new(SystemTime::now()));
        watch_for_end(pid, state.clone(), changed.clone());

        let model = model::Watch::new(&command);
        Ok(Session {
            name,
            id,
            command,
            checkout: Checkout::find(&cwd),
            cwd,
            env: env.clone(),
            pid: Some(pid),
            state,
            activity: None,
            changed,
            conversation: None,
            rollouts: None,
            telling: notify::Telling::default(),
            bell: false,
            unseen_copies: 0,
            screen_watch: ScreenWatch::default(),
            front: None,
            front_group: None,
            front_checked: Instant::now(),
            task: None,
            goal: None,
            about: TaskBrief::default(),
            reminded: false,
            reporter: None,
            reporter_job: None,
            named_after_program: false,
            name_given: false,
            title: claude_title::Watch::default(),
            typed_agent: None,
            subagents: 0,
            model,
            shown: report::Shown::default(),
            stopped_idle: false,
            stopped_to_move: false,
            closed: Vec::new(),
            changes: Vec::new(),
            start_from: None,
            term,
        })
    }

    /// Makes a task: a session for `claude -p` runs of `spec`, with `args`,
    /// whose screen shows what Claude does, and whose runs add to
    /// `spending`. It starts at rest; [`Session::prompt`] gives it its
    /// prompt. Given a `conversation`, its runs carry it on.
    #[allow(clippy::too_many_arguments)]
    pub fn task(
        id: String,
        name: String,
        spec: TaskSpec,
        args: Vec<String>,
        cwd: PathBuf,
        env: BTreeMap<String, String>,
        spending: Arc<Spending>,
        conversation: Option<String>,
    ) -> Session {
        let term = Arc::new(Term::without_terminal());
        let state = Arc::new(Mutex::new(State::Running));
        let command = task_command(&spec);
        let task = Task::new(
            spec,
            args,
            cwd.clone(),
            env.clone(),
            term.clone(),
            state.clone(),
            spending,
            conversation,
        );
        Session {
            name,
            id,
            command,
            checkout: Checkout::find(&cwd),
            cwd,
            env,
            pid: None,
            state,
            activity: None,
            changed: Arc::new(Mutex::new(SystemTime::now())),
            conversation: None,
            rollouts: None,
            telling: notify::Telling::default(),
            bell: false,
            unseen_copies: 0,
            screen_watch: ScreenWatch::default(),
            front: Some(Front::Task),
            front_group: None,
            front_checked: Instant::now(),
            task: Some(task),
            goal: None,
            about: TaskBrief::default(),
            reminded: false,
            reporter: None,
            reporter_job: None,
            named_after_program: false,
            name_given: false,
            title: claude_title::Watch::default(),
            typed_agent: None,
            subagents: 0,
            model: model::Watch::default(),
            shown: report::Shown::default(),
            stopped_idle: false,
            stopped_to_move: false,
            closed: Vec::new(),
            changes: Vec::new(),
            start_from: None,
            term,
        }
    }

    /// A session written down before a restart, in the list while it waits
    /// its turn to start again from `saved`, with no program yet: its screen
    /// says so to whoever looks. It keeps its `id` once it has started.
    pub fn to_start(id: String, saved: SavedSession) -> Session {
        let name = printable::line(&saved.name).into_owned();
        let session = Session::unstarted(id, saved);
        session.term.show(
            format!("\x1b[2mstarting {name} again after crystal's restart…\x1b[0m").as_bytes(),
        );
        session
    }

    /// A session written down before a restart that couldn't start again
    /// from `saved`, for the reason `why`: it stays in the list, its screen
    /// saying why, to start again once that's put right, or to kill.
    pub fn failed_to_start(id: String, saved: SavedSession, why: &str) -> Session {
        let mut session = Session::unstarted(id, saved);
        session.fail_to_start(why);
        session
    }

    fn unstarted(id: String, saved: SavedSession) -> Session {
        let front = match saved.task {
            Some(_) => Some(Front::Task),
            None => front::of_command(&saved.command),
        };
        Session {
            name: saved.name.clone(),
            id,
            command: saved.command.clone(),
            checkout: Checkout::find(&saved.cwd),
            cwd: saved.cwd.clone(),
            env: BTreeMap::new(),
            pid: None,
            state: Arc::new(Mutex::new(State::Starting)),
            activity: None,
            changed: Arc::new(Mutex::new(SystemTime::now())),
            conversation: saved.conversation.clone(),
            rollouts: None,
            telling: notify::Telling::default(),
            bell: false,
            unseen_copies: 0,
            screen_watch: ScreenWatch::default(),
            front,
            front_group: None,
            front_checked: Instant::now(),
            task: None,
            goal: saved.goal.clone(),
            about: saved.about.clone(),
            reminded: false,
            reporter: None,
            reporter_job: None,
            named_after_program: false,
            name_given: saved.name_given,
            title: claude_title::Watch::default(),
            typed_agent: None,
            subagents: 0,
            model: model::Watch::new(&saved.command),
            shown: report::Shown::default(),
            stopped_idle: false,
            stopped_to_move: false,
            closed: Vec::new(),
            changes: Vec::new(),
            start_from: Some(saved),
            term: Arc::new(Term::without_terminal()),
        }
    }

    /// Whether the session waits its turn to start again after a restart.
    pub fn is_starting(&self) -> bool {
        *self.state.lock().unwrap() == State::Starting
    }

    /// Whether the session is yet to start again after a restart, or
    /// couldn't: see [`State::is_unstarted`].
    pub fn is_unstarted(&self) -> bool {
        self.start_from.is_some()
    }

    /// The session waiting its turn couldn't start, for the reason `why`:
    /// its screen says so, and the start it waited for is over.
    pub fn fail_to_start(&mut self, why: &str) {
        let (name, shown) = (printable::line(&self.name), printable::line(why));
        let said = format!(
            "\r\n\x1b[1mcrystal couldn't start {name} again after the restart:\x1b[0m\r\n\r\n  \
             {shown}\r\n\r\nIt's kept as it was: once that's put right, Enter in crystal's \
             sidebar or `crystal respawn {name}` starts it again, and `crystal kill {name}` \
             lets it go.\r\n"
        );
        self.term.show(said.as_bytes());
        self.term.close();
        *self.state.lock().unwrap() = State::Failed {
            why: why.to_string(),
        };
        *self.changed.lock().unwrap() = SystemTime::now();
    }

    pub fn is_task(&self) -> bool {
        self.task.is_some()
    }

    /// Gives a task a prompt to run: its own to start with, then
    /// follow-ups, which carry its conversation on.
    pub fn prompt(&self, text: &str) -> Result<()> {
        let task = self
            .task
            .as_ref()
            .with_context(|| format!("{} isn't a task", self.name))?;
        ensure!(self.is_running(), "{} has ended", self.name);
        task.run(text)
    }

    /// What its agent is asking the user, while it's stopped until they
    /// answer: a background task's permission, or an agent in a terminal
    /// asking something, as its hooks, its screen or its own report say.
    /// Anything typed into it then would land in the question. An agent
    /// waiting only because its turn ended with its task open is at its
    /// prompt, and takes a message as ever.
    pub fn blocked(&self) -> Option<String> {
        if let Some(task) = &self.task {
            let asking = task.asking()?;
            return Some(match asking.gist.as_str() {
                "" => format!("asking to use {}", asking.tool),
                gist => format!("asking to use {}: {gist}", asking.tool),
            });
        }
        if self.activity != Some(Activity::Waiting) {
            return None;
        }
        let at_its_prompt = self
            .goal
            .as_ref()
            .is_some_and(|goal| goal.is_open() && goal.waiting);
        if at_its_prompt {
            return None;
        }
        let said = self.reporter.as_ref().and_then(|reporter| {
            let message = reporter.message.as_deref()?.trim();
            (!message.is_empty()).then(|| format!("waiting on the user ({message})"))
        });
        Some(said.unwrap_or_else(|| "asking the user something".to_string()))
    }

    /// A task's last answer, and what it has come to.
    pub fn result(&self) -> Result<TaskResult> {
        let name = &self.name;
        let task = self
            .task
            .as_ref()
            .with_context(|| format!("{name} isn't a task"))?;
        ensure!(
            !task.is_working(),
            "{name} is still working: `crystal wait {name}` for it first"
        );
        task.result()
            .with_context(|| format!("{name} has no answer yet"))
    }

    /// How a task's latest run ended, once it has and the session has seen
    /// it end: `None` while a run is going on, before any has ended, and for
    /// a session that isn't a task. A run that ended since [`Session::check`]
    /// last looked isn't over for the session yet: its agent still reads as
    /// working, and whoever acts on the end, like a flow taking a step's
    /// answer and marking it seen, would act on it before the session turns
    /// done, which would undo what they did.
    pub fn finished_run(&self) -> Option<TaskResult> {
        let task = self.task.as_ref()?;
        if task.is_working() || !task.caught_up() {
            return None;
        }
        task.result()
    }

    /// Whether the session is a background task in the middle of a run.
    pub fn in_a_run(&self) -> bool {
        self.task.as_ref().is_some_and(Task::is_working)
    }

    /// The background task this session runs, or why there's none.
    fn background(&self) -> Result<&Task> {
        let name = &self.name;
        self.task.as_ref().with_context(|| {
            format!(
                "{name} isn't a background task: answer it in its pane, or with `crystal send-keys {name}`"
            )
        })
    }

    /// Answers the permission a background task is waiting on the user
    /// for.
    pub fn answer(&self, answer: Answer, message: Option<&str>) -> Result<()> {
        let task = self.background()?;
        ensure!(self.is_running(), "{} has ended", self.name);
        task.answer(answer, message)
            .with_context(|| format!("{} can't take an answer", self.name))
    }

    /// Stops the run a background task is in the middle of.
    pub fn interrupt(&self) -> Result<()> {
        let task = self.background()?;
        ensure!(self.is_running(), "{} has ended", self.name);
        task.interrupt()
            .with_context(|| format!("{} can't be interrupted", self.name))
    }

    /// A task started again after a restart: its screen shows what it did
    /// before, from Claude Code's transcript of its conversation, and it
    /// waits at rest for a follow-up, which carries its conversation on.
    pub fn came_back(&mut self) {
        if let Some(task) = &self.task {
            task.draw_kept();
            self.on_agent_event(AgentEvent::Started);
        }
    }

    /// Makes the session a task: its agent was asked to do something, which
    /// stays open until the task is closed done or failed.
    pub fn give_task(&mut self, goal: TaskInfo) {
        self.goal = Some(goal);
    }

    /// Has the session be about the pull request and the issue `brief`
    /// names, task or not; its acceptance criteria are its task's alone.
    pub fn set_about(&mut self, brief: &TaskBrief) {
        self.about = TaskBrief {
            accept: Vec::new(),
            ..brief.clone()
        };
    }

    /// Closes the session's task, saying how it went: `state` is done,
    /// failed or cancelled. Gives back the task as the project's history
    /// keeps it.
    pub fn close_task(&mut self, state: TaskState, summary: &str) -> Result<TaskRecord> {
        let name = &self.name;
        let goal = self.goal.as_mut().with_context(|| {
            format!("{name} has no task: it wasn't started with something to do")
        })?;
        let closed = seconds_since_epoch(SystemTime::now());
        goal.outcome = Some(TaskOutcome::new(state, summary, closed));
        // A task that waited on the user asks nothing of them any more.
        if std::mem::take(&mut goal.waiting) && self.activity == Some(Activity::Waiting) {
            self.set_activity(Some(Activity::Idle));
        }
        *self.changed.lock().unwrap() = SystemTime::now();
        Ok(self.task_record().expect("the task was just closed"))
    }

    /// Cancels the session's task, if it's open, saying `why`, and gives it
    /// back as the project's history keeps it.
    pub fn cancel_task(&mut self, why: &str) -> Option<TaskRecord> {
        // A task's run that started before it was last looked at would open
        // it again once it was: it's looked at first.
        self.check_runs();
        if !self.goal.as_ref().is_some_and(TaskInfo::is_open) {
            return None;
        }
        self.close_task(TaskState::Cancelled, why).ok()
    }

    /// The session's task, as `crystal tasks show` shows it.
    pub fn task_view(&self) -> Option<TaskView> {
        let record = self.task_record()?;
        Some(TaskView {
            state: record.state(),
            record,
            session_state: Some(self.state.lock().unwrap().clone()),
            asking: self.task.as_ref().and_then(Task::asking),
            cost_usd: self.task.as_ref().map(Task::cost_usd),
            context: self.task.as_ref().and_then(Task::context),
        })
    }

    /// The id of the session's task, if it has one.
    pub fn task_id(&self) -> Option<u64> {
        self.goal.as_ref()?.id
    }

    /// Whether the session has a task with no number yet.
    pub fn task_unnumbered(&self) -> bool {
        self.goal.as_ref().is_some_and(|goal| goal.id.is_none())
    }

    /// Gives the session's task the number `number`, unless it has one.
    pub fn number_task(&mut self, number: u64) {
        if let Some(goal) = &mut self.goal {
            goal.id.get_or_insert(number);
        }
    }

    /// Whether to remind the agent, as it ends a turn, that its task is
    /// still open: once, so one that has its reasons, like waiting on the
    /// user, isn't held up turn after turn. A background task has no turns
    /// to end; its runs close it.
    pub fn remind_of_task(&mut self) -> bool {
        let open = self
            .goal
            .as_ref()
            .is_some_and(|goal| goal.outcome.is_none() && !goal.background);
        if !open || self.reminded {
            return false;
        }
        self.reminded = true;
        true
    }

    /// The tasks that have closed of themselves since this was last asked,
    /// for the daemon to write down.
    pub fn take_closed(&mut self) -> Vec<TaskRecord> {
        std::mem::take(&mut self.closed)
    }

    /// What has happened to it since this was last asked, in order, for
    /// the daemon to tell.
    pub fn take_changes(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.changes)
    }

    /// The session's task as `crystal tasks` lists it, if it has one.
    pub fn task_record(&self) -> Option<TaskRecord> {
        let goal = self.goal.as_ref()?;
        let worktree = self.checkout.as_ref().map(Checkout::worktree);
        let project = match &worktree {
            Some(worktree) => worktree.project.clone(),
            None => crate::project::of(&self.cwd).name,
        };
        Some(TaskRecord {
            id: goal.id,
            goal: goal.goal.clone(),
            session: self.name.clone(),
            project,
            branch: worktree.and_then(|worktree| worktree.branch),
            background: goal.background,
            backlog: goal.backlog,
            pending: false,
            waiting: goal.waiting,
            created: goal.created,
            outcome: goal.outcome.clone(),
            artifacts: Vec::new(),
            brief: goal.brief.clone(),
        })
    }

    /// Where the session runs: the directory its project is found from.
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// What git is in the middle of in the worktree the session runs in,
    /// if anything: a task there isn't done while it lasts.
    pub fn worktree_in_progress(&self) -> Option<InProgress> {
        self.checkout.as_ref()?.worktree().in_progress
    }

    /// The top of the worktree the session runs in, or outside git, where
    /// it runs.
    pub fn checkout_top(&self) -> PathBuf {
        match &self.checkout {
            Some(checkout) => checkout.worktree().path,
            None => self.cwd.clone(),
        }
    }

    /// The main worktree of the repository the session runs in, if it runs
    /// in one.
    pub fn project_path(&self) -> Option<&Path> {
        self.checkout.as_ref().map(Checkout::project_path)
    }

    pub fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }

    /// What the distiller can read of what the session did: a task's runs,
    /// or the transcript Claude Code keeps of its conversation. Nothing of
    /// any other program.
    pub fn material(&self) -> Option<Material> {
        if let Some(task) = &self.task {
            return Some(Material::Task {
                record: task.record(),
                conversation: task.conversation(),
            });
        }
        if agents::program_name(&self.command) != Some("claude") {
            return None;
        }
        let transcript = self.conversation.as_ref()?.transcript.clone()?;
        Some(Material::Transcript(transcript))
    }

    pub fn is_running(&self) -> bool {
        *self.state.lock().unwrap() == State::Running
    }

    /// The process of its program while it runs: a background task's
    /// `claude`, while it has one.
    pub fn running_pid(&self) -> Option<u32> {
        let pid = self.task.as_ref().map_or(self.pid, Task::pid);
        pid.filter(|_| self.is_running())
    }

    pub fn info(&self) -> SessionInfo {
        let now = SystemTime::now();
        // The model read is the agent's in front: a shell or another
        // program has none. One the agent reported knows better.
        let read_model = match &self.front {
            Some(Front::Shell { .. } | Front::Program { .. }) => None,
            _ => {
                let read = self.model.model().map(String::from);
                read.or_else(|| self.task.as_ref().and_then(Task::model))
            }
        };
        // An agent that reports for itself is in front by the name it gave,
        // whatever its process is called.
        let front = match &self.reporter {
            Some(reporter) => Some(Front::Agent {
                program: reporter.agent.clone(),
                name: reporter.agent.clone(),
            }),
            None => self.front.clone(),
        };
        SessionInfo {
            front,
            name: self.name.clone(),
            id: self.id.clone(),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            pid: self.task.as_ref().map_or(self.pid, Task::pid),
            state: self.state.lock().unwrap().clone(),
            activity: self.activity,
            worktree: self.checkout.as_ref().map(Checkout::worktree),
            changed: seconds_since_epoch(*self.changed.lock().unwrap()),
            task: self.goal.clone(),
            asking: self.task.as_ref().and_then(Task::asking),
            reporter: self.reporter.clone(),
            // An agent gone before it was seen in front can't have taken
            // its subagents with it.
            subagents: match &self.front {
                Some(front) if !front.is_agent() => 0,
                _ => self.subagents,
            },
            model: self.shown.model(now).map(String::from).or(read_model),
            line: self.shown.line(now).map(String::from),
            row: self.shown.row(now),
            stopped_idle: self.stopped_idle,
            bell: self.bell,
            unseen_copies: self.unseen_copies,
            context: self.task.as_ref().and_then(Task::context),
            output_waits: self.term.output_waits(),
        }
    }

    /// How long the session's agent has sat idle at its prompt: its turn
    /// seen, nobody watching it or typing into it, and nothing changed. Only
    /// an agent that can come back where it was, in its conversation or with
    /// the command it gave, counts; never a task, nor one with its task
    /// open, which waits on the user. `None` when it isn't idle that way.
    pub fn idle_for(&self) -> Option<Duration> {
        if !self.is_running() || self.task.is_some() || self.activity != Some(Activity::Idle) {
            return None;
        }
        if self.goal.as_ref().is_some_and(TaskInfo::is_open) {
            return None;
        }
        let agent = match &self.reporter {
            Some(reporter) => reporter.resume.is_some(),
            None => {
                let in_front = self.front.as_ref().is_some_and(Front::is_agent);
                in_front && self.conversation.is_some()
            }
        };
        if !agent {
            return None;
        }
        let untouched = self.term.untouched_for()?;
        let unchanged = self.changed.lock().unwrap().elapsed().unwrap_or_default();
        Some(untouched.min(unchanged))
    }

    /// Stops the session's agent, which has sat idle: it stays in the list,
    /// to start again in its conversation.
    pub fn stop_idle(&mut self) {
        self.stopped_idle = true;
        self.stop();
    }

    /// Stops the session to start it again in another worktree, under its
    /// id: its task stays open meanwhile, to carry on there.
    pub fn stop_to_move(&mut self) {
        self.stopped_to_move = true;
        self.stop();
    }

    /// Takes what the session's agent says about itself with `crystal
    /// report`, sent by `source` and numbered `seq` when it says so. Its
    /// first report of what it's doing takes the session's status over; a
    /// resume command alone needs it to hold the session already, so that a
    /// command never outlives the agent it's for. False when the report was
    /// passed over: it came after a later one from its source, or it lets
    /// go of a session another source holds.
    pub fn take_report(
        &mut self,
        report: AgentReport,
        source: Option<String>,
        seq: Option<u64>,
    ) -> Result<bool> {
        if !self.shown.status_in_order(source.as_deref(), seq)? {
            return Ok(false);
        }
        match report {
            AgentReport::State {
                agent,
                state,
                message,
                resume,
            } => {
                if let Some(argv) = &resume {
                    report::check_resume(argv)?;
                }
                let agent = match agent {
                    Some(agent) => report::checked_agent(agent)?,
                    None => self.agent_name(),
                };
                if self.reporter.is_none() {
                    self.changes.push(Change::Claimed);
                    self.reporter_job = self.term.foreground_group();
                }
                let reporter = self.reporter.get_or_insert(Reporter {
                    agent: agent.clone(),
                    message: None,
                    resume: None,
                    source: None,
                });
                reporter.agent = agent;
                if source.is_some() {
                    reporter.source = source;
                }
                // Shown on the user's screen, and in their notifications.
                reporter.message = message
                    .map(|message| printable::line(&message).trim().to_string())
                    .filter(|message| !message.is_empty());
                if resume.is_some() {
                    reporter.resume = resume;
                }
                self.on_agent_event(report::event(state, self.activity));
            }
            AgentReport::Resume { agent, argv } => {
                report::check_resume(&argv)?;
                let agent = agent.map(report::checked_agent).transpose()?;
                let reporter = self.reporter.as_mut().with_context(|| {
                    format!(
                        "no agent reports for {} yet: say what it's doing along with the \
                         command, like `crystal report idle -- <command>`",
                        self.name
                    )
                })?;
                if let Some(agent) = agent {
                    reporter.agent = agent;
                }
                reporter.resume = Some(argv);
            }
            AgentReport::Release => {
                let holder = self.reporter.as_ref().and_then(|r| r.source.as_deref());
                if let (Some(holder), Some(source)) = (holder, source.as_deref())
                    && holder != source
                {
                    return Ok(false);
                }
                self.release();
            }
        }
        Ok(true)
    }

    /// Takes what `crystal report --line` or `--model` puts on the
    /// session's row. False when it came late, after a later report from
    /// the same source, and was passed over.
    pub fn take_metadata(&mut self, metadata: &Metadata) -> Result<bool> {
        self.shown.take(metadata, SystemTime::now())
    }

    /// Takes the model the agent's hooks say it runs on.
    pub fn heard_model(&mut self, model: &str) {
        self.model.heard(model);
    }

    /// Reads what the agent's conversation has gained since the last look,
    /// for a switch of its model, while it runs in front.
    pub fn check_model(&mut self) {
        if !self.is_running() || self.task.is_some() {
            return;
        }
        if matches!(
            self.front,
            Some(Front::Shell { .. } | Front::Program { .. })
        ) {
            return;
        }
        let transcript = self
            .conversation
            .as_ref()
            .and_then(|c| c.transcript.clone());
        if let Some(transcript) = transcript {
            self.model.look(&transcript);
        }
    }

    /// The name of an agent that reports without giving one: the one it
    /// gave before, or else what's in front in the terminal.
    fn agent_name(&self) -> String {
        if let Some(reporter) = &self.reporter {
            return reporter.agent.clone();
        }
        let front = self
            .front
            .clone()
            .or_else(|| front::of_command(&self.command));
        front.map_or_else(|| "agent".to_string(), |front| front.word().to_string())
    }

    /// The agent that reports for itself lets go of the session: crystal
    /// reads what it's doing for itself again, from nothing, and the
    /// agent's command won't resume it.
    fn release(&mut self) {
        let Some(reporter) = self.reporter.take() else {
            return;
        };
        self.reporter_job = None;
        self.changes.push(Change::Released {
            agent: reporter.agent,
        });
        self.screen_watch = ScreenWatch::default();
        if self.activity.is_some() {
            self.set_activity(None);
            *self.changed.lock().unwrap() = SystemTime::now();
        }
    }

    /// Whether an agent that reports for itself holds the session.
    pub fn is_claimed(&self) -> bool {
        self.reporter.is_some()
    }

    /// crystal named the session after its program: its first prompt can
    /// name it.
    pub fn mark_named_after_program(&mut self) {
        self.named_after_program = true;
    }

    /// Whether the session's first prompt can name it: crystal named it
    /// after its program, and nothing has named it since.
    pub fn is_named_after_program(&self) -> bool {
        self.named_after_program
    }

    /// Its name was given now, by the user or a script, or comes from its
    /// prompt: nothing names it after this but a rename.
    pub fn keep_name(&mut self) {
        self.named_after_program = false;
    }

    /// The user or a script gave it the name it has now as it started: a
    /// rename in Claude Code leaves it.
    pub fn keep_given_name(&mut self) {
        self.keep_name();
        self.name_given = true;
    }

    /// The user or a script renamed it: the name is kept through a rename in
    /// Claude Code, and given to Claude Code with the next prompt.
    pub fn renamed(&mut self) {
        self.keep_given_name();
        self.title.give(&self.name);
    }

    /// Whether the user or a script gave it its name.
    pub fn name_given(&self) -> bool {
        self.name_given
    }

    /// Looks at the name Claude Code keeps for its agent's conversation,
    /// while Claude Code runs in front: the name, when Claude Code was
    /// given a new one since the last look.
    pub fn check_title(&mut self) -> Option<String> {
        if !self.is_running() || self.task.is_some() {
            return None;
        }
        let front = self
            .front
            .clone()
            .or_else(|| front::of_command(&self.command));
        let claude = matches!(front, Some(Front::Agent { program, .. }) if program == "claude");
        let conversation = self.conversation.as_ref().filter(|_| claude)?;
        let transcript = conversation.transcript.as_ref()?;
        self.title.look(transcript, &conversation.id)
    }

    /// The name to give Claude Code's conversation as the user sends it a
    /// prompt, once: the one the user renamed the session to, when Claude
    /// Code hasn't it yet.
    pub fn title_to_give(&mut self) -> Option<String> {
        self.title.take_giving()
    }

    /// Works out what the agent is doing from what it just reported. A
    /// turn that ends with the session's task still open is a question for
    /// the user, and the task waits on them until the agent works again.
    pub fn on_agent_event(&mut self, event: AgentEvent) {
        self.subagents = subagents_after(self.subagents, event);
        let turn_ended = event == AgentEvent::TurnEnded
            || (event == AgentEvent::StillIdle && self.activity == Some(Activity::Working));
        let mut activity = next_activity(self.activity, event, self.is_watched());
        if let Some(goal) = self.goal.as_mut().filter(|goal| goal.is_open()) {
            if turn_ended && tasks_on() {
                if !std::mem::replace(&mut goal.waiting, true) {
                    self.changes.push(Change::TaskWaiting);
                }
                activity = Some(Activity::Waiting);
            } else if matches!(event, AgentEvent::TurnStarted | AgentEvent::ToolFinished) {
                goal.waiting = false;
            }
        }
        if activity != self.activity {
            self.set_activity(activity);
            *self.changed.lock().unwrap() = SystemTime::now();
        }
    }

    /// Takes what the agent is doing now, noting the change for the daemon
    /// to tell. A conversation the agent works on a turn in has something
    /// to pick up again, which only one with no file to look for needs
    /// told.
    fn set_activity(&mut self, activity: Option<Activity>) {
        if activity == Some(Activity::Working)
            && let Some(conversation) = self.conversation.as_mut()
            && conversation.transcript.is_none()
        {
            conversation.prompted = true;
        }
        if activity != self.activity {
            self.changes.push(Change::Activity {
                from: self.activity,
                to: activity,
            });
            self.activity = activity;
        }
    }

    /// Keeps up with what the session's agent is doing, a task's from its
    /// runs, any other program's from its screen, and fails a task whose
    /// session has ended under it.
    pub fn check(&mut self) {
        if self.task.is_some() {
            self.check_runs();
        } else {
            self.check_screen();
        }
        self.check_bell();
        self.check_copies();
        self.fail_task_if_ended();
    }

    /// Keeps up with a task's runs since it last looked.
    fn check_runs(&mut self) {
        let events = self.task.as_mut().map(Task::events).unwrap_or_default();
        let mut tools = self.task.as_mut().map(Task::tools_used);
        for event in events {
            // The tools a run used are told after it started and before
            // it ended.
            if event == AgentEvent::TurnEnded {
                self.tell_tools(tools.take());
            }
            // How a run ended closes its task first: a task that stays
            // open waits on the user.
            self.follow_runs(event);
            self.on_agent_event(event);
        }
        self.tell_tools(tools);
    }

    fn tell_tools(&mut self, tools: Option<Vec<ToolUse>>) {
        let tools = tools.into_iter().flatten();
        self.changes.extend(tools.map(Change::ToolUsed));
    }

    /// Marks the session when its program has rung the bell while nobody
    /// was watching, until someone looks at it: a viewer passes on the
    /// bells of a session it shows itself.
    fn check_bell(&mut self) {
        let rang = self.term.take_bells() > 0;
        if rang && !self.bell && !self.term.is_watched() {
            self.bell = true;
            self.changes.push(Change::Bell);
        }
    }

    /// Counts the copies its program asked its terminal for while nobody was
    /// watching. Nobody put them on the clipboard: there was nobody to see
    /// what they were, and putting one there later would take the place of
    /// whatever the user copied meanwhile. A viewer puts what a session it
    /// shows copies there itself. A task's screen shows what crystal draws
    /// of Claude's work, not a program's own output, and copies nothing.
    fn check_copies(&mut self) {
        let unseen = self.term.take_unseen_copies();
        if unseen > 0 && self.task.is_none() && programs_copy() {
            self.unseen_copies = self.unseen_copies.saturating_add(unseen);
            self.changes.push(Change::UnseenCopy);
        }
    }

    /// Keeps up with a task's runs, noting each that starts and ends, and a
    /// permission it comes to ask for. A background task closes itself when
    /// a run ends, from what Claude said at the end, and opens again when a
    /// follow-up starts another. A run the user stopped leaves it open.
    fn follow_runs(&mut self, event: AgentEvent) {
        let Some(task) = &self.task else {
            return;
        };
        match event {
            AgentEvent::TurnStarted => {
                let prompt = task.last_prompt();
                if let Some(goal) = &mut self.goal
                    && goal.outcome.is_some()
                {
                    goal.outcome = None;
                    self.changes.push(Change::Reopened);
                }
                self.changes.push(Change::RunStarted { prompt });
            }
            AgentEvent::Asking => {
                if let Some(asking) = task.asking() {
                    self.changes.push(Change::Asking(asking));
                }
            }
            AgentEvent::TurnEnded => {
                let Some(result) = task.result() else {
                    return;
                };
                let interrupted = task.was_interrupted();
                self.changes.push(Change::RunEnded(result.clone()));
                let open = self.goal.as_ref().is_some_and(TaskInfo::is_open);
                if !open || interrupted {
                    return;
                }
                let summary = result.text.lines().next().unwrap_or("");
                let state = if result.failed {
                    TaskState::Failed
                } else {
                    TaskState::Done
                };
                if let Ok(record) = self.close_task(state, summary) {
                    self.closed.push(record);
                }
            }
            _ => {}
        }
    }

    /// Fails the session's task when the session has ended with it still
    /// open: nobody is left who could close it.
    fn fail_task_if_ended(&mut self) {
        let open = self.goal.as_ref().is_some_and(TaskInfo::is_open);
        // One yet to start again hasn't ended: its task goes on once it
        // has. Nor has one stopped to start again in another worktree.
        let ending = !self.is_running() && !self.is_unstarted() && !self.stopped_to_move;
        if !open || !ending || !tasks_on() {
            return;
        }
        let why = format!("its session ended: {}", self.state.lock().unwrap());
        if let Ok(record) = self.close_task(TaskState::Failed, &why) {
            self.closed.push(record);
        }
    }

    /// Looks at what's in front in the terminal, when its job has changed
    /// or it's been a while. Cheap otherwise: one question to the terminal.
    /// An agent that reports for itself and left without letting go of the
    /// session lets go of it once the shell is back in front.
    pub fn check_front(&mut self) {
        if self.task.is_some() || !self.is_running() {
            return;
        }
        let Some(group) = self.term.foreground_group() else {
            return;
        };
        let same_job = self.front_group == Some(group);
        if !same_job || self.front_checked.elapsed() >= FRONT_RECHECK {
            self.front_group = Some(group);
            self.front_checked = Instant::now();
            if let Some(front) = front::of_process(group) {
                self.set_front(front);
            }
        }
        let at_a_shell = matches!(self.front, Some(Front::Shell { .. }));
        if at_a_shell && self.reporter_job != Some(group) {
            self.release();
        }
    }

    /// Takes what's in front now. An agent that leaves the front takes what
    /// it was doing with it: the shell it gives the terminal back to isn't
    /// working or waiting on anyone, and has no subagents. One typed into
    /// the shell takes its conversation too, which a restart mustn't bring
    /// back once the user has quit it.
    fn set_front(&mut self, front: Front) {
        if self.front.as_ref() == Some(&front) {
            return;
        }
        let agent_left = self.front.as_ref().is_some_and(Front::is_agent);
        if agent_left && self.activity.is_some() {
            self.set_activity(None);
            *self.changed.lock().unwrap() = SystemTime::now();
        }
        if agent_left {
            self.subagents = 0;
            self.model.forget();
            if self.typed_agent.take().is_some() {
                self.conversation = None;
            }
        }
        self.screen_watch = ScreenWatch::default();
        self.front = Some(front);
    }

    /// Reads what the agent is doing off the screen, and takes it as an
    /// event when that has changed. Only while an agent is in front: a
    /// shell or any other program can print an agent's words. An agent
    /// that reports for itself knows better than its screen.
    fn check_screen(&mut self) {
        if !self.is_running() || self.is_claimed() {
            return;
        }
        let Some(Front::Agent { program, .. }) = &self.front else {
            return;
        };
        // A screen that says nothing either way leaves the look as it was.
        let Some(looks) = self.term.looks(program) else {
            return;
        };
        if let Some(event) = self.screen_watch.update(looks) {
            self.on_agent_event(event);
        }
    }

    /// Whether what `agent`'s hooks report is about this session: it is,
    /// unless another agent is in front, which `agent` was started by.
    pub fn reports_for(&self, agent: &str) -> bool {
        match &self.front {
            Some(Front::Agent { program, .. }) => agent_rules::current().same_agent(agent, program),
            _ => true,
        }
    }

    /// Why crystal reads the session's agent the way it does: what's in
    /// front, whether its screen is read, and the rules tried on it, those
    /// of `agent` when it's given, or else of the agent in front.
    pub fn explain_screen(&self, agent: Option<&str>) -> ScreenExplained {
        let in_front = match &self.front {
            Some(Front::Agent { program, .. }) => Some(program.as_str()),
            _ => None,
        };
        let not_read = if self.task.is_some() {
            Some("it's a background task, which says what it's doing itself".to_string())
        } else if !self.is_running() {
            Some("it has ended".to_string())
        } else if self.is_claimed() {
            Some("its agent says what it's doing itself, with `crystal report`".to_string())
        } else if in_front.is_none() {
            let what = self.front.as_ref().map_or("nothing yet", Front::word);
            Some(format!("no agent is in front, but {what}"))
        } else {
            None
        };
        let rules = agent
            .or(in_front)
            .filter(|_| self.task.is_none())
            .map(|agent| {
                let (rows, title, progress) = self.term.screen_for_rules();
                let screen = agent_rules::Input {
                    rows: &rows,
                    title: &title,
                    progress: &progress,
                };
                agent_rules::current().for_program(agent).explain(&screen)
            });
        ScreenExplained {
            session: self.name.clone(),
            front: self.front.clone(),
            not_read,
            watch: self.screen_watch.looks().into(),
            candidate: self.screen_watch.candidate().map(Into::into),
            activity: self.activity,
            rules,
        }
    }

    /// Whether someone is watching the session: it's shown somewhere, and
    /// not only in TUIs whose terminals have all lost the focus.
    fn is_watched(&self) -> bool {
        notify::watching(self.term.is_watched())
    }

    /// Something to tell the user, when the session has come to need them,
    /// and gone on needing them `after` how long: its agent is asking them
    /// something, or is done with a turn nobody watched.
    pub fn notice(&mut self, after: impl FnOnce() -> Duration) -> Option<Notice> {
        let watched = self.is_watched();
        // A turn that ended while nobody watched has been seen once
        // someone does, say as the TUI's terminal gets the focus back.
        if watched {
            self.seen();
        }
        let now = self.activity.filter(|_| self.is_running());
        let tell = self.telling.update(now, watched, Instant::now(), after);
        match now {
            Some(activity) if tell => Some(Notice::about(&self.info(), activity)),
            _ => None,
        }
    }

    pub fn set_conversation(&mut self, conversation: Conversation) {
        self.conversation = Some(conversation);
    }

    /// Takes the conversation `agent`'s hooks name, `agent` by its program.
    /// One that isn't the session's own program was typed into its shell:
    /// a restart resumes it by typing its command for that conversation.
    pub fn set_hooked_conversation(&mut self, agent: &str, mut conversation: Conversation) {
        // An agent's hooks may call it otherwise than its program is
        // called, as Cursor's do `cursor-agent`.
        let own = agents::program_name(&self.command)
            .is_some_and(|program| agent_rules::current().same_agent(program, agent));
        self.typed_agent = (!own).then(|| agent.to_string());
        // A hook that doesn't say where the conversation is kept, as
        // Codex's needn't, leaves the file known already, and the turns
        // seen in it.
        if let Some(known) = self.conversation.take()
            && known.id == conversation.id
        {
            if conversation.transcript.is_none() {
                conversation.transcript = known.transcript;
            }
            conversation.prompted |= known.prompted;
        }
        // Named as it works, the agent is working in it.
        if conversation.transcript.is_none() && self.activity == Some(Activity::Working) {
            conversation.prompted = true;
        }
        self.conversation = Some(conversation);
    }

    /// Whether `agent`, by its program, can be what's in front: it is, or
    /// it's the session's own program and nothing has been seen in front
    /// yet.
    pub fn may_run(&self, agent: &str) -> bool {
        match &self.front {
            Some(Front::Agent { program, .. }) => program == agent,
            Some(_) => false,
            None => agents::program_name(&self.command) == Some(agent),
        }
    }

    /// The id of the agent's conversation, once it's known.
    pub fn conversation_id(&self) -> Option<&str> {
        let conversation = self.conversation.as_ref()?;
        Some(conversation.id.as_str())
    }

    /// Has the session look for its Codex conversation in `rollouts`, for
    /// as long as it doesn't know it.
    pub fn look_for_conversation_in(&mut self, rollouts: Rollouts) {
        self.rollouts = Some(rollouts);
    }

    /// Where a Codex session is looking for its conversation, while it runs
    /// and doesn't know it yet.
    pub fn looking_for_conversation(&self) -> Option<&Rollouts> {
        if self.conversation.is_some() || !self.is_running() {
            return None;
        }
        self.rollouts.as_ref()
    }

    /// Looks for a Codex session's conversation, while it runs and isn't
    /// known yet. `claimed` are the conversations other sessions are in;
    /// `looking` says where every Codex session still looking is, and when
    /// it started, its own place among them.
    pub fn find_conversation(&mut self, claimed: &[&str], looking: &[(PathBuf, SystemTime)]) {
        let Some(ours) = self.looking_for_conversation() else {
            return;
        };
        let rivals: Vec<SystemTime> = looking
            .iter()
            .filter(|(cwd, started)| cwd == ours.cwd() && *started != ours.started())
            .map(|(_, started)| *started)
            .collect();
        let Some(rollouts) = &mut self.rollouts else {
            return;
        };
        if let Some(conversation) = rollouts.look(claimed, &rivals) {
            self.conversation = Some(conversation);
        }
    }

    /// What it takes to start the session again after a restart, while it
    /// runs, or while it's yet to start again. A program that has ended
    /// stays ended.
    pub fn saved(&self) -> Option<SavedSession> {
        if self.is_running() || self.is_unstarted() {
            Some(self.launch())
        } else {
            None
        }
    }

    /// What it takes to start the session's program again: its name,
    /// command and directory, and the agent's conversation to pick up, or
    /// the command that resumes an agent that reports for itself, or one
    /// typed into the session's shell (see [`restart_with`]).
    pub fn launch(&self) -> SavedSession {
        // One yet to start again starts as it was written down, under the
        // name it has now.
        if let Some(saved) = &self.start_from {
            return SavedSession {
                name: self.name.clone(),
                name_given: self.name_given,
                ..saved.clone()
            };
        }
        // A task's conversation comes from Claude's own events, which need
        // no transcript file to resume it.
        let conversation = match &self.task {
            Some(task) => task.conversation().map(|id| Conversation {
                id,
                transcript: None,
                prompted: false,
            }),
            None => self.conversation.clone(),
        };
        let (conversation, resume) = restart_with(
            conversation,
            self.reporter.as_ref(),
            self.typed_agent.as_deref(),
            self.front.as_ref(),
        );
        SavedSession {
            name: self.name.clone(),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            conversation,
            task: self.task.as_ref().map(|task| task.spec().clone()),
            goal: self.goal.clone(),
            resume,
            about: self.about.clone(),
            name_given: self.name_given,
            moved: None,
        }
    }

    /// Hands the session over to the next daemon: what it takes to carry it
    /// on, its screen, and its terminal or a task's pipes, kept open across
    /// the exec. Its readers must have been stopped
    /// ([`handover::stop_reading`]). Its program's state comes back held,
    /// to hold until the exec: a program that ends meanwhile is reaped only
    /// with it held, so it's either handed over as having ended, or handed
    /// over unreaped, for the next daemon to wait for.
    pub fn hand_over(&self) -> io::Result<(Handed, MutexGuard<'_, State>)> {
        let task = self.task.as_ref().map(Task::hand_over).transpose()?;
        let (screen, ended, pty) = self.term.hand_over()?;
        let state = self.state.lock().unwrap();
        let start_from = self.start_from.as_ref().map(|_| StartFrom {
            saved: self.launch(),
            failed: match &*state {
                State::Failed { why } => Some(why.clone()),
                _ => None,
            },
        });
        let handed = Handed {
            name: self.name.clone(),
            id: self.id.clone(),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            env: self.env.clone(),
            pid: self.pid,
            // What a crystal from before sessions waited to start again
            // can read: one that has ended.
            state: match &*state {
                State::Starting | State::Failed { .. } => State::Exited { code: 1 },
                state => state.clone(),
            },
            activity: self.activity,
            changed: *self.changed.lock().unwrap(),
            looks: self.screen_watch.looks(),
            front: self.front.clone(),
            conversation: self.conversation.clone(),
            rollouts: self.rollouts.clone(),
            told: self.telling.told,
            goal: self.goal.clone(),
            about: self.about.clone(),
            reminded: self.reminded,
            reporter: self.reporter.clone(),
            reporter_job: self.reporter_job,
            named_after_program: self.named_after_program,
            name_given: self.name_given,
            title: self.title.clone(),
            typed_agent: self.typed_agent.clone(),
            subagents: self.subagents,
            model: self.model.clone(),
            shown: self.shown.clone(),
            stopped_idle: self.stopped_idle,
            start_from,
            screen,
            ended,
            pty,
            task,
        };
        Ok((handed, state))
    }

    /// Carries on a session the last daemon handed over: its screen as it
    /// was, its terminal read again, and its program waited for. A task's
    /// runs add to `spending`. Fails when what it was handed isn't open.
    pub fn adopt(handed: Handed, spending: &Arc<Spending>) -> Result<Session> {
        // One yet to start again waits again, or says again why it couldn't.
        if let Some(StartFrom { saved, failed }) = handed.start_from {
            return Ok(match failed {
                Some(why) => Session::failed_to_start(handed.id, saved, &why),
                None => Session::to_start(handed.id, saved),
            });
        }
        let pty = handed
            .pty
            .map(|fd| handover::inherit(fd).map(|fd| Pty::new(File::from(fd))))
            .transpose()
            .with_context(|| format!("{}'s terminal wasn't handed over", handed.name))?;
        let screen = vt::Screen::restored(&handed.screen);
        // What the program wrote before the handover is on the screen, but
        // not in the ring.
        let term = Arc::new(Term::with_screen(pty, screen, false));
        let state = Arc::new(Mutex::new(handed.state.clone()));
        let changed = Arc::new(Mutex::new(handed.changed));
        let task = match handed.task {
            Some(task) => Some(
                Task::adopt(
                    task,
                    handed.cwd.clone(),
                    handed.env.clone(),
                    term.clone(),
                    state.clone(),
                    spending.clone(),
                )
                .with_context(|| format!("{}'s claude wasn't handed over", handed.name))?,
            ),
            None => None,
        };
        if handed.ended {
            term.close();
        } else {
            term.start_pumping();
        }
        if let (Some(pid), State::Running, None) = (handed.pid, &handed.state, &task) {
            watch_for_end(pid, state.clone(), changed.clone());
        }
        Ok(Session {
            name: handed.name,
            id: handed.id,
            command: handed.command,
            checkout: Checkout::find(&handed.cwd),
            cwd: handed.cwd,
            env: handed.env,
            pid: handed.pid,
            state,
            activity: handed.activity,
            changed,
            screen_watch: ScreenWatch::seeing(handed.looks),
            front: handed.front,
            front_group: None,
            front_checked: Instant::now(),
            conversation: handed.conversation,
            rollouts: handed.rollouts,
            telling: notify::Telling::after(handed.told),
            bell: false,
            unseen_copies: 0,
            task,
            goal: handed.goal,
            about: handed.about,
            reminded: handed.reminded,
            reporter: handed.reporter,
            reporter_job: handed.reporter_job,
            named_after_program: handed.named_after_program,
            name_given: handed.name_given,
            title: handed.title,
            typed_agent: handed.typed_agent,
            subagents: handed.subagents,
            model: handed.model,
            shown: handed.shown,
            stopped_idle: handed.stopped_idle,
            stopped_to_move: false,
            closed: Vec::new(),
            changes: Vec::new(),
            start_from: None,
            term,
        })
    }

    /// Someone has just looked at the session.
    pub fn seen(&mut self) {
        if self.activity == Some(Activity::Done) {
            self.set_activity(Some(Activity::Idle));
        }
        self.bell = false;
    }

    pub fn term(&self) -> Arc<Term> {
        self.term.clone()
    }

    /// Hangs up on everything the session started, the way closing a
    /// terminal window does, and kills whatever is still there after
    /// [`STOP_GRACE`].
    pub fn stop(&self) {
        if let Some(task) = &self.task {
            return task.stop();
        }
        let Some(pid) = self.pid.filter(|_| self.is_running()) else {
            return;
        };
        signal_group(pid, libc::SIGHUP);
        let state = self.state.clone();
        thread::spawn(move || {
            thread::sleep(STOP_GRACE);
            if *state.lock().unwrap() == State::Running {
                signal_group(pid, libc::SIGKILL);
            }
        });
    }
}

/// The daemon's end of a session's PTY: the screen the program has drawn,
/// the clients watching it, and the way in.
pub struct Term {
    /// `None` for a task, which has no terminal: crystal draws what Claude
    /// does on the screen itself.
    pty: Option<Pty>,
    screen: Mutex<Screen>,
    /// The thread reading the program's output, for a handover to wait for
    /// once it has stopped it.
    pump: Mutex<Option<JoinHandle<()>>>,
    /// How many waits for a line on the screen are looking at it.
    output_waits: AtomicU32,
}

/// The daemon's side of a PTY, its master side: read for what the program
/// writes, written with what it's sent, and where its size is set.
struct Pty {
    master: File,
    /// Held while writing, so that two writers' bytes never interleave.
    writing: Mutex<()>,
}

impl Pty {
    fn new(master: File) -> Pty {
        Pty {
            master,
            writing: Mutex::new(()),
        }
    }

    /// The master side portable_pty opened, on a descriptor of crystal's
    /// own, which a handover can keep open: portable_pty's closes as it's
    /// dropped. Its writer is never made, since dropping it would send the
    /// program an end of file.
    fn of(master: Box<dyn MasterPty + Send>) -> Result<Pty> {
        let fd = master
            .as_raw_fd()
            .context("the terminal has no descriptor")?;
        // SAFETY: `master` keeps the descriptor open until it's dropped,
        // after this borrow.
        let ours = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
        Ok(Pty::new(File::from(ours)))
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let _writing = self.writing.lock().unwrap();
        (&self.master).write_all(bytes)
    }

    /// Tells the kernel, and so the program, the terminal's new size.
    fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCSWINSZ reads one winsize, which `size` is.
        let set = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) };
        if set == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The process group in front: the job its keys go to.
    fn foreground_group(&self) -> Option<i32> {
        // SAFETY: tcgetpgrp only asks.
        match unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) } {
            group if group > 0 => Some(group),
            _ => None,
        }
    }
}

struct Screen {
    /// What the program has drawn, and the history. It answers the
    /// program's questions to its terminal too: viewers only draw, so the
    /// answers come from here, whether anyone's watching or not.
    vt: vt::Screen,
    viewers: Vec<Viewer>,
    /// Told each time there's output, but not watching: see
    /// [`Term::listen`].
    listeners: Vec<SyncSender<()>>,
    /// The program has closed its end: there will be no more output.
    ended: bool,
    /// When someone last had anything to do with the session: typed into
    /// it, or started or stopped watching it. Its program's own output
    /// doesn't count: an agent at its prompt can redraw as it likes.
    touched: Instant,
    /// The copies the program asked its terminal for while nobody was
    /// watching, since they were last taken.
    unseen_copies: u32,
    /// The output lately, with when it came, for `crystal read --since`.
    ring: OutputRing,
    /// The [`WRITES`] count as the program last wrote to the main screen,
    /// the one a cold restart shows again: it has changed since whoever
    /// last kept it when this has. The alternate screen doesn't count, so
    /// a program that stays on it isn't gone back from again and again to
    /// keep a main screen that hasn't changed.
    main_written: u64,
}

/// How many times any session's program has written to its screen: each
/// time, the count goes up, so the count a screen keeps is never another's.
static WRITES: AtomicU64 = AtomicU64::new(0);

struct Viewer {
    id: u64,
    feed: SyncSender<Arc<[u8]>>,
    /// A program watching, like `crystal observe`, not the user: the
    /// session isn't watched for it.
    program: bool,
}

impl Viewer {
    /// Whether it's the user watching.
    fn is_user(&self) -> bool {
        !self.program
    }
}

/// A new viewer's start: the screen as it is now, with the history ahead
/// of it if asked for, then everything the program writes after it.
pub struct Watch {
    pub id: u64,
    pub screen: Vec<u8>,
    /// `None` once the program has ended.
    pub feed: Option<Receiver<Arc<[u8]>>>,
}

impl Term {
    /// A screen of [`UNSEEN_SIZE`], until a viewer gives it another size.
    fn new(pty: Option<Pty>) -> Term {
        let (rows, cols) = UNSEEN_SIZE;
        Term::with_screen(pty, vt::Screen::answering(rows, cols), true)
    }

    /// A terminal showing `vt`: a new program's, `from_the_start`, or one
    /// that has written to it before, as a handover carries on.
    fn with_screen(pty: Option<Pty>, vt: vt::Screen, from_the_start: bool) -> Term {
        Term {
            pty,
            screen: Mutex::new(Screen {
                vt,
                viewers: Vec::new(),
                listeners: Vec::new(),
                ended: false,
                touched: Instant::now(),
                unseen_copies: 0,
                ring: OutputRing::new(now_ms(), from_the_start),
                main_written: 0,
            }),
            pump: Mutex::default(),
            output_waits: AtomicU32::new(0),
        }
    }

    /// Reads the program's output, on a thread of its own, until the
    /// program closes the terminal or a handover stops it.
    fn start_pumping(self: &Arc<Term>) {
        if self.pty.is_none() {
            return;
        }
        let term = self.clone();
        *self.pump.lock().unwrap() = Some(thread::spawn(move || term.pump()));
    }

    /// The screen to hand over once a handover has stopped the reading of
    /// the program's output, whether there will be more, and while there
    /// may be, its terminal, kept open across the exec.
    fn hand_over(&self) -> io::Result<(vt::Saved, bool, Option<RawFd>)> {
        let pump = self.pump.lock().unwrap().take();
        if let Some(pump) = pump {
            let _ = pump.join();
        }
        let mut screen = self.screen.lock().unwrap();
        let pty = match &self.pty {
            Some(pty) if !screen.ended => Some(handover::keep_across_exec(pty.master.as_fd())?),
            _ => None,
        };
        Ok((screen.vt.save(), screen.ended, pty))
    }

    /// A screen with no program behind it, for a task to draw on.
    pub fn without_terminal() -> Term {
        Term::new(None)
    }

    /// Starts showing the session to a new viewer. With `with_history`, the
    /// viewer's screen gets the history too, so that it can scroll back
    /// through output from before it came. A `program` watching, like
    /// `crystal observe`, isn't the user: the session isn't watched or
    /// touched for it.
    pub fn watch(&self, with_history: bool, program: bool) -> Watch {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut screen = self.screen.lock().unwrap();
        let snapshot = screen.vt.state_formatted(with_history);
        if !program {
            screen.touched = Instant::now();
        }
        let feed = (!screen.ended).then(|| {
            let (feed, rx) = mpsc::sync_channel(VIEWER_BACKLOG);
            screen.viewers.push(Viewer { id, feed, program });
            rx
        });
        Watch {
            id,
            screen: snapshot,
            feed,
        }
    }

    /// A signal each time the program writes something, to look at the
    /// screen again: never more than one waiting, however much it writes,
    /// and none after it has ended. Unlike a viewer, a listener isn't
    /// watching, so the session isn't seen for it.
    pub fn listen(&self) -> Receiver<()> {
        let mut screen = self.screen.lock().unwrap();
        let (signal, listener) = mpsc::sync_channel(1);
        if !screen.ended {
            screen.listeners.push(signal);
        }
        listener
    }

    /// Runs `wait`, a wait for a line on the screen, counted in
    /// [`Term::output_waits`] while it lasts.
    pub fn waiting_for_output<T>(&self, wait: impl FnOnce() -> T) -> T {
        self.output_waits.fetch_add(1, Ordering::Relaxed);
        let waited = wait();
        self.output_waits.fetch_sub(1, Ordering::Relaxed);
        waited
    }

    /// How many waits for a line on the screen, `crystal wait --output`'s,
    /// are looking at it.
    pub fn output_waits(&self) -> u32 {
        self.output_waits.load(Ordering::Relaxed)
    }

    /// A number that changes each time the program writes to the main
    /// screen, and is never another screen's: whether what
    /// [`Term::kept_screen`] keeps has changed since it was last kept.
    pub fn main_written(&self) -> u64 {
        self.screen.lock().unwrap().main_written
    }

    /// The main screen and its history, to show again above the program
    /// started again after a cold restart: see [`vt::Screen::kept`].
    pub fn kept_screen(&self) -> vt::Saved {
        self.screen.lock().unwrap().vt.kept()
    }

    /// The screen, one string per row, after the last `history` rows of
    /// the history.
    pub fn recent_rows(&self, history: usize) -> Vec<String> {
        self.screen.lock().unwrap().vt.recent_rows(history)
    }

    /// What the screen says `agent`, the program in front, is doing: `None`
    /// when it says nothing either way.
    pub fn looks(&self, agent: &str) -> Option<Looks> {
        let (rows, title, progress) = self.screen_for_rules();
        agent_screen::read(agent, &rows, &title, &progress)
    }

    /// The screen as the rules that read agents take it: its rows, its
    /// title and the progress its program reports.
    pub fn screen_for_rules(&self) -> (Vec<String>, String, String) {
        let screen = self.screen.lock().unwrap();
        let progress = screen.vt.progress().to_string();
        (screen.vt.rows(false), screen.vt.title(), progress)
    }

    /// What pressing `key`, a key's name or some text, sends the program:
    /// the way it asked for keys, which changes what a key named to
    /// `send-keys` sends.
    pub fn keystrokes(&self, key: &str) -> Vec<u8> {
        keys::keystrokes(key, &self.screen.lock().unwrap().vt)
    }

    /// Whether the program has asked for pastes to be marked as pastes.
    pub fn wants_bracketed_paste(&self) -> bool {
        self.screen.lock().unwrap().vt.bracketed_paste()
    }

    /// How many columns wide the screen is.
    pub fn columns(&self) -> u16 {
        self.screen.lock().unwrap().vt.size().1
    }

    /// What's on the screen, one string per row, after the rows of the
    /// history with `with_history`.
    pub fn rows(&self, with_history: bool) -> Vec<String> {
        self.screen.lock().unwrap().vt.rows(with_history)
    }

    /// Has the screen keep `lines` rows of history from now on: see
    /// [`vt::Screen::keep_history`].
    pub fn keep_history(&self, lines: usize) {
        self.screen.lock().unwrap().vt.keep_history(lines);
    }

    /// What's on the screen as `crystal read` asks for it, as
    /// [`vt::Screen::lines`] reads it. With `since`, in milliseconds since
    /// the Unix epoch, only what the program wrote from then on, laid out
    /// on a screen of its own of this one's size, history and all; or the
    /// history and the screen whole, when the output kept can't tell what
    /// came since then from what came before.
    pub fn read(&self, history: bool, unwrap: bool, ansi: bool, since: Option<u64>) -> Vec<String> {
        let screen = self.screen.lock().unwrap();
        let Some(since) = since else {
            return screen.vt.lines(history, unwrap, ansi);
        };
        let Some(output) = screen.ring.since(since) else {
            return screen.vt.lines(true, unwrap, ansi);
        };
        let (rows, cols) = screen.vt.size();
        // Laid out off the lock: the program goes on writing meanwhile.
        drop(screen);
        let mut replayed = vt::Screen::new(rows, cols);
        replayed.process(&output);
        replayed.lines(true, unwrap, ansi)
    }

    /// The process group in front in the terminal: the job its keys go to.
    /// `None` without a terminal, or when the terminal won't say.
    pub fn foreground_group(&self) -> Option<i32> {
        self.pty.as_ref()?.foreground_group()
    }

    /// Whether the user is watching it, not only a program.
    pub fn is_watched(&self) -> bool {
        self.screen
            .lock()
            .unwrap()
            .viewers
            .iter()
            .any(Viewer::is_user)
    }

    /// The screen's size, rows by columns.
    pub fn size(&self) -> (u16, u16) {
        self.screen.lock().unwrap().vt.size()
    }

    /// How many times the program rang the bell since the last call.
    pub fn take_bells(&self) -> u32 {
        self.screen.lock().unwrap().vt.take_bells()
    }

    /// How many times the program asked its terminal to copy something
    /// while nobody was watching, since the last call.
    pub fn take_unseen_copies(&self) -> u32 {
        std::mem::take(&mut self.screen.lock().unwrap().unseen_copies)
    }

    pub fn unwatch(&self, id: u64) {
        let mut screen = self.screen.lock().unwrap();
        let user = screen
            .viewers
            .iter()
            .any(|viewer| viewer.id == id && viewer.is_user());
        screen.viewers.retain(|viewer| viewer.id != id);
        if user {
            screen.touched = Instant::now();
        }
    }

    /// How long it's been since anyone had anything to do with the
    /// session: `None` while someone watches it.
    pub fn untouched_for(&self) -> Option<Duration> {
        let screen = self.screen.lock().unwrap();
        let watched = screen.viewers.iter().any(Viewer::is_user);
        (!watched).then(|| screen.touched.elapsed())
    }

    /// Writes `bytes` to the program, as from the user: typed, pasted or
    /// sent.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.screen.lock().unwrap().touched = Instant::now();
        match &self.pty {
            Some(pty) => pty.write(bytes),
            None => Err(io::Error::other("a task takes no keys")),
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let mut screen = self.screen.lock().unwrap();
        screen.vt.resize(rows, cols);
        if let Some(pty) = &self.pty {
            pty.resize(rows, cols)?;
        }
        Ok(())
    }

    /// Draws `output` on the screen as if a program had written it: how a
    /// task shows what Claude does.
    pub fn show(&self, output: &[u8]) {
        self.take_output(output);
    }

    /// Says there will be no more output: viewers see the end, and new ones
    /// get the last screen and nothing after it.
    pub fn close(&self) {
        let mut screen = self.screen.lock().unwrap();
        screen.ended = true;
        screen.viewers.clear();
        screen.listeners.clear();
    }

    /// Reads the program's output until it closes the terminal, and answers
    /// the program's questions to its terminal. A handover stops it with
    /// the rest unread, for the next daemon to read.
    fn pump(&self) {
        let Some(pty) = &self.pty else {
            return;
        };
        let mut buf = [0; 16 * 1024];
        loop {
            match handover::readers().read(&pty.master, &mut buf) {
                Ok(Got::Bytes(n)) => {
                    let replies = self.take_output(&buf[..n]);
                    // The terminal's own answers, not the user's.
                    if !replies.is_empty() {
                        let _ = pty.write(&replies);
                    }
                }
                Ok(Got::Stopped) => return,
                Ok(Got::End) | Err(_) => break,
            }
        }
        self.close();
    }

    /// Keeps the screen up to date with `output` and passes it on to every
    /// viewer. Returns what the program asked its terminal, to answer.
    fn take_output(&self, output: &[u8]) -> Vec<u8> {
        let mut screen = self.screen.lock().unwrap();
        let alternate = screen.vt.alternate_screen();
        screen.vt.process(output);
        screen.ring.push(output, now_ms());
        if !alternate || !screen.vt.alternate_screen() {
            screen.main_written = WRITES.fetch_add(1, Ordering::Relaxed) + 1;
        }
        // Viewers get the same output, so that their own screens keep the
        // same history.
        let chunk: Arc<[u8]> = output.into();
        // A viewer that's gone, or too far behind to catch up, is dropped
        // rather than holding up the program.
        screen
            .viewers
            .retain(|viewer| viewer.feed.try_send(chunk.clone()).is_ok());
        // What the program copies, a viewer's own screen sees, and puts on
        // the clipboard; with none, nobody does.
        let watched = screen.viewers.iter().any(Viewer::is_user);
        if screen.vt.take_copied().is_some() && !watched {
            screen.unseen_copies = screen.unseen_copies.saturating_add(1);
        }
        // One signal waiting is enough: the listener looks at the screen as
        // it is then.
        screen.listeners.retain(|listener| {
            !matches!(listener.try_send(()), Err(TrySendError::Disconnected(_)))
        });
        screen.vt.take_replies()
    }
}

/// Whether tasks are on, by the config as it is now: with them off, an
/// open task is left as it is.
fn tasks_on() -> bool {
    tasks::enabled(&Config::load().unwrap_or_default())
}

/// Whether what programs copy goes to the clipboard, by the config as it is
/// now: with it off, a copy nobody saw is nothing to tell.
fn programs_copy() -> bool {
    Config::load().unwrap_or_default().clipboard.allow_programs
}

/// `time` as seconds since the Unix epoch, which is how it travels.
fn seconds_since_epoch(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// What a session's agent is doing after `event`, given what it was doing
/// before and whether anyone is watching the session.
fn next_activity(before: Option<Activity>, event: AgentEvent, watched: bool) -> Option<Activity> {
    let after = match event {
        AgentEvent::Started => Activity::Idle,
        AgentEvent::TurnStarted | AgentEvent::ToolFinished => Activity::Working,
        AgentEvent::Asking => Activity::Waiting,
        AgentEvent::TurnEnded => Activity::Done,
        // Only news if we thought it was still working: a turn the user
        // cut short reports no end.
        AgentEvent::StillIdle if before == Some(Activity::Working) => Activity::Done,
        AgentEvent::StillIdle => return before,
        // A subagent's start and end are the agent's work, not its turn.
        AgentEvent::SubagentStarted | AgentEvent::SubagentStopped => return before,
        AgentEvent::Named => return before,
    };
    // A turn that ends while someone's watching has been seen.
    if after == Activity::Done && watched {
        Some(Activity::Idle)
    } else {
        Some(after)
    }
}

/// How many subagents an agent that had `count` running has after `event`:
/// one more as one starts, one fewer as one stops, never below none, and
/// none when it starts afresh.
fn subagents_after(count: u32, event: AgentEvent) -> u32 {
    match event {
        AgentEvent::SubagentStarted => count + 1,
        AgentEvent::SubagentStopped => count.saturating_sub(1),
        AgentEvent::Started => 0,
        _ => count,
    }
}

/// How `ls` shows a task's command: the `claude -p` it runs, with its own
/// arguments after the prompt.
fn task_command(spec: &TaskSpec) -> Vec<String> {
    let mut command = vec!["claude".to_string(), "-p".to_string(), spec.prompt.clone()];
    command.extend(spec.args.iter().cloned());
    command
}

/// Waits, on a thread of its own, for the program `pid` to end, then notes
/// how it ended in `state`. It's reaped only with `state` held, which a
/// handover holds from before it looks at the session until the exec: so
/// the handover either sees that it ended, or hands it over unreaped, for
/// the next daemon to wait for.
fn watch_for_end(pid: u32, state: Arc<Mutex<State>>, changed: Arc<Mutex<SystemTime>>) {
    thread::spawn(move || {
        let _ = handover::wait_for_end(pid);
        let mut state = state.lock().unwrap();
        *state = match handover::reap(pid) {
            Ok(status) => ended(&status.into()),
            Err(_) => State::Exited { code: 1 },
        };
        *changed.lock().unwrap() = SystemTime::now();
    });
}

fn ended(status: &ExitStatus) -> State {
    match status.signal() {
        // macOS spells it "Terminated: 15"; keep the name.
        Some(signal) => State::Signaled {
            signal: signal.split(':').next().unwrap_or(signal).to_string(),
        },
        None => State::Exited {
            code: status.exit_code(),
        },
    }
}

/// The child leads its own session and process group, so signalling the
/// group reaches whatever it started too.
pub fn signal_group(pid: u32, signal: libc::c_int) {
    // SAFETY: kill only sends a signal. The caller has checked the child
    // hasn't been reaped yet, so the group id still belongs to it.
    unsafe {
        libc::kill(-(pid as libc::pid_t), signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Activity::*;

    #[test]
    fn a_session_handed_over_comes_back_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open(&dir.path().join("crystal.sock")).unwrap();
        let spending = Arc::new(Spending::new(db));
        let mut screen = vt::Screen::answering(5, 20);
        screen.process(b"\x1b]2;working\x07bye\r\n");
        let goal = TaskInfo {
            id: Some(12),
            goal: "fix the login".into(),
            background: false,
            backlog: None,
            waiting: true,
            created: 1,
            outcome: None,
            brief: Default::default(),
        };
        // What its agent put on its row, for a while yet.
        let mut shown = report::Shown::default();
        let metadata = Metadata {
            line: Some("deploying".into()),
            model: Some("pi-large".into()),
            ttl_secs: Some(600),
            ..Metadata::default()
        };
        shown.take(&metadata, SystemTime::now()).unwrap();
        let handed = Handed {
            name: "agent".into(),
            id: "id-1".into(),
            command: vec!["claude".into()],
            cwd: dir.path().to_path_buf(),
            env: BTreeMap::new(),
            pid: Some(4242),
            state: State::Exited { code: 3 },
            activity: Some(Waiting),
            changed: UNIX_EPOCH + Duration::from_secs(100),
            looks: Looks::Waiting,
            front: Some(Front::Shell { name: "zsh".into() }),
            conversation: Some(Conversation {
                id: "conv-1".into(),
                transcript: None,
                prompted: false,
            }),
            rollouts: None,
            told: Some(Waiting),
            goal: Some(goal.clone()),
            about: TaskBrief::default(),
            reminded: true,
            reporter: Some(Reporter {
                agent: "pi".into(),
                message: Some("approve the deploy".into()),
                resume: Some(vec!["pi".into(), "--resume".into(), "s 1".into()]),
                source: None,
            }),
            reporter_job: Some(4242),
            named_after_program: true,
            name_given: false,
            title: claude_title::Watch::default(),
            typed_agent: Some("codex".into()),
            subagents: 2,
            model: model::Watch::new(&["claude".into(), "--model".into(), "opus".into()]),
            shown,
            stopped_idle: false,
            start_from: None,
            screen: screen.save(),
            ended: true,
            pty: None,
            task: None,
        };
        // Through the file it's handed over in.
        let handed: Handed =
            serde_json::from_str(&serde_json::to_string(&handed).unwrap()).unwrap();
        assert!(handed.saved().is_none(), "it had ended");
        assert!(handed.processes().is_empty());

        let mut session = Session::adopt(handed, &spending).unwrap();
        let info = session.info();
        assert_eq!((info.name.as_str(), info.id.as_str()), ("agent", "id-1"));
        assert_eq!(info.state, State::Exited { code: 3 });
        assert_eq!(info.activity, Some(Waiting));
        assert_eq!(info.changed, 100);
        assert_eq!(info.task, Some(goal));
        assert_eq!(session.conversation_id(), Some("conv-1"));
        assert_eq!(session.term().rows(false)[0], "bye");
        assert!(
            session.term().watch(false, false).feed.is_none(),
            "it had ended"
        );
        // Reminded of its task once already, it isn't again.
        assert!(!session.remind_of_task());
        // Still held by the agent that reports for itself, which resumes
        // with its own command, and still named after its program.
        assert!(session.is_claimed());
        assert_eq!(info.reporter.unwrap().agent, "pi");
        let resume = session.launch().resume.unwrap();
        assert_eq!(resume, ["pi", "--resume", "s 1"]);
        assert!(session.is_named_after_program());
        assert_eq!(session.subagents, 2);
        // With the shell in front, there are none to show.
        assert_eq!(info.subagents, 0);
        // What its agent put on its row stays, the model it reported over
        // the one its command gave.
        assert_eq!(info.line.as_deref(), Some("deploying"));
        assert_eq!(info.model.as_deref(), Some("pi-large"));
        assert_eq!(session.model.model(), Some("opus"));
    }

    /// A Claude Code session as it was written down before a restart, in
    /// `dir`, with its task open.
    fn written_down(dir: &Path) -> SavedSession {
        SavedSession {
            name: "fixer".into(),
            command: vec!["claude".into()],
            cwd: dir.to_path_buf(),
            conversation: Some(Conversation {
                id: "conv-1".into(),
                transcript: None,
                prompted: false,
            }),
            task: None,
            goal: Some(TaskInfo {
                id: Some(7),
                goal: "fix the login".into(),
                background: false,
                backlog: None,
                waiting: false,
                created: 1,
                outcome: None,
                brief: Default::default(),
            }),
            resume: None,
            about: Default::default(),
            name_given: false,
            moved: None,
        }
    }

    #[test]
    fn only_what_goes_on_the_main_screen_changes_what_a_restart_would_show_again() {
        let term = Term::without_terminal();
        assert_eq!(term.main_written(), 0);
        term.show(b"$ ls\r\nCargo.toml\r\n");
        let main = term.main_written();
        assert!(main > 0);
        // Onto the alternate screen, the main one stays as it was.
        term.show(b"\x1b[?1049hfull screen");
        let entered = term.main_written();
        assert!(entered > main, "it began on the main screen");
        term.show(b"\x1b[Hredrawn");
        assert_eq!(term.main_written(), entered);
        assert!(term.kept_screen().output.contains("Cargo.toml"));
        term.show(b"\x1b[?1049l$ ");
        assert!(term.main_written() > entered);
        let kept = term.kept_screen().output;
        assert!(kept.contains("$ ls") && !kept.contains("redrawn"), "{kept}");
        // Another screen's count is never the same.
        let other = Term::without_terminal();
        other.show(b"$ ls\r\n");
        assert_ne!(other.main_written(), term.main_written());
    }

    #[test]
    fn a_wait_for_output_is_counted_while_it_lasts() {
        let term = Term::without_terminal();
        assert_eq!(term.waiting_for_output(|| term.output_waits()), 1);
        assert_eq!(term.output_waits(), 0);
    }

    #[test]
    fn a_session_yet_to_start_again_is_written_down_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let saved = written_down(dir.path());
        let mut session = Session::to_start("id-1".into(), saved.clone());
        let info = session.info();
        assert_eq!(info.state, State::Starting);
        assert_eq!((info.name.as_str(), info.id.as_str()), ("fixer", "id-1"));
        assert_eq!(info.pid, None);
        assert_eq!(info.task, saved.goal);
        assert_eq!(info.front.unwrap().word(), "claude");
        assert!(session.is_starting() && session.is_unstarted());
        assert!(!session.is_running());
        assert_eq!(session.saved(), Some(saved.clone()));
        assert!(session.term().rows(false)[0].contains("starting fixer again"));
        // Its task waits for it to start, rather than failing.
        session.check();
        assert!(session.take_closed().is_empty());
        assert!(session.goal.as_ref().unwrap().is_open());
        // Renamed meanwhile, it starts under its new name, which is given.
        session.name = "login".into();
        session.renamed();
        assert_eq!(session.launch().name, "login");
        assert!(session.launch().name_given);
        assert_eq!(session.launch().conversation, saved.conversation);
        // A name given before the restart stays given.
        let given = SavedSession {
            name_given: true,
            ..saved
        };
        let session = Session::to_start("id-2".into(), given.clone());
        assert!(session.name_given());
        assert_eq!(session.saved(), Some(given));
    }

    #[test]
    fn a_session_that_couldn_t_start_again_says_why_and_stays_written_down() {
        let dir = tempfile::tempdir().unwrap();
        let saved = written_down(dir.path());
        let mut session = Session::to_start("id-1".into(), saved.clone());
        let watching = session.term().watch(false, false).feed.unwrap();
        session.fail_to_start("command not found: claude");
        let why = "command not found: claude".to_string();
        assert_eq!(session.info().state, State::Failed { why });
        assert_eq!(session.info().status(), "couldn't start");
        assert!(!session.is_starting() && session.is_unstarted());
        assert_eq!(session.saved(), Some(saved));
        // Whoever was looking sees why, and that nothing more is coming.
        let said: Vec<u8> = watching.iter().flat_map(|chunk| chunk.to_vec()).collect();
        let said = String::from_utf8_lossy(&said);
        assert!(said.contains("command not found: claude"), "{said}");
        assert!(said.contains("crystal respawn fixer"), "{said}");
        session.check();
        assert!(session.take_closed().is_empty(), "its task stays open");
    }

    #[test]
    fn a_session_yet_to_start_again_is_handed_over_as_ended_to_an_older_crystal() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open(&dir.path().join("crystal.sock")).unwrap();
        let spending = Arc::new(Spending::new(db));
        let saved = written_down(dir.path());
        let failed = Session::failed_to_start("id-1".into(), saved.clone(), "gone");
        let starting = Session::to_start("id-2".into(), saved.clone());
        for (session, state) in [
            (failed, State::Failed { why: "gone".into() }),
            (starting, State::Starting),
        ] {
            let (handed, _state) = session.hand_over().unwrap();
            let json = serde_json::to_value(&handed).unwrap();
            // A crystal from before reads it as a session that has ended.
            assert_eq!(json["state"], serde_json::json!({"exited": {"code": 1}}));
            let handed: Handed = serde_json::from_value(json).unwrap();
            assert_eq!(handed.saved(), Some(saved.clone()));
            assert!(handed.processes().is_empty());
            let adopted = Session::adopt(handed, &spending).unwrap();
            assert_eq!(adopted.info().state, state);
            assert_eq!(adopted.id, session.id);
            assert_eq!(adopted.launch(), saved);
        }
    }

    /// A shell session's, handed over, that ended with `front` in front,
    /// in Claude Code's conversation `conv-1`, typed into it by hand, with
    /// two subagents running.
    fn typed_claude(front: Front) -> Session {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open(&dir.path().join("crystal.sock")).unwrap();
        let spending = Arc::new(Spending::new(db));
        let handed = Handed {
            name: "shell".into(),
            id: "id-1".into(),
            command: vec!["zsh".into()],
            cwd: dir.path().to_path_buf(),
            env: BTreeMap::new(),
            pid: None,
            state: State::Exited { code: 0 },
            activity: Some(Working),
            changed: UNIX_EPOCH,
            looks: Looks::default(),
            front: Some(front),
            conversation: Some(Conversation {
                id: "conv-1".into(),
                transcript: None,
                prompted: false,
            }),
            rollouts: None,
            told: None,
            goal: None,
            about: TaskBrief::default(),
            reminded: false,
            reporter: None,
            reporter_job: None,
            named_after_program: false,
            name_given: false,
            title: claude_title::Watch::default(),
            typed_agent: Some("claude".into()),
            subagents: 2,
            model: model::Watch::default(),
            shown: report::Shown::default(),
            stopped_idle: false,
            start_from: None,
            screen: vt::Screen::answering(5, 20).save(),
            ended: true,
            pty: None,
            task: None,
        };
        Session::adopt(handed, &spending).unwrap()
    }

    fn claude() -> Front {
        Front::Agent {
            program: "claude".into(),
            name: "Claude Code".into(),
        }
    }

    #[test]
    fn an_agent_typed_into_the_shell_takes_its_conversation_with_it_as_it_leaves() {
        let mut session = typed_claude(claude());
        session.set_front(Front::Shell { name: "zsh".into() });
        assert_eq!(session.conversation_id(), None);
        assert_eq!(session.info().subagents, 0);
        assert_eq!(session.info().activity, None);
        assert_eq!(session.launch().resume, None);
    }

    #[test]
    fn the_session_s_own_agent_s_conversation_isn_t_one_typed_in() {
        let mut session = typed_claude(claude());
        let conversation = Conversation {
            id: "conv-2".into(),
            transcript: None,
            prompted: false,
        };
        session.set_hooked_conversation("claude", conversation.clone());
        assert_eq!(session.typed_agent.as_deref(), Some("claude"));
        session.command = vec!["/usr/local/bin/claude".into()];
        session.set_hooked_conversation("claude", conversation);
        assert_eq!(session.typed_agent, None);
        assert_eq!(session.launch().conversation.unwrap().id, "conv-2");
    }

    #[test]
    fn a_hook_that_doesn_t_say_where_the_conversation_is_kept_leaves_it_known() {
        let mut session = typed_claude(claude());
        let kept = |id: &str, file: Option<&str>| Conversation {
            id: id.into(),
            transcript: file.map(PathBuf::from),
            prompted: false,
        };
        session.set_hooked_conversation("claude", kept("conv-1", Some("/t/conv-1.jsonl")));
        session.set_hooked_conversation("claude", kept("conv-1", None));
        assert_eq!(
            session.conversation,
            Some(kept("conv-1", Some("/t/conv-1.jsonl")))
        );
        // Another conversation's file isn't this one's. With none, it has
        // had a turn once it's named as its agent works.
        session.set_hooked_conversation("claude", kept("conv-2", None));
        let worked = Conversation {
            prompted: true,
            ..kept("conv-2", None)
        };
        assert_eq!(session.conversation, Some(worked));
    }

    #[test]
    fn a_conversation_with_no_file_counts_once_its_agent_works_in_it() {
        let mut session = typed_claude(claude());
        session.on_agent_event(AgentEvent::Started);
        let named = Conversation {
            id: "k-1".into(),
            transcript: None,
            prompted: false,
        };
        session.set_hooked_conversation("claude", named.clone());
        let resumes = |session: &Session| session.conversation.as_ref().unwrap().can_resume();
        assert!(!resumes(&session), "never sent a prompt");
        session.on_agent_event(AgentEvent::Named);
        assert!(!resumes(&session));
        session.on_agent_event(AgentEvent::TurnStarted);
        assert!(resumes(&session));
        // Named again, it keeps the turns it had.
        session.on_agent_event(AgentEvent::TurnEnded);
        session.set_hooked_conversation("claude", named);
        assert!(resumes(&session));
    }

    #[test]
    fn an_agent_s_hooks_may_name_it_otherwise_than_its_program() {
        let mut session = typed_claude(claude());
        session.command = vec!["/usr/local/bin/cursor-agent".into()];
        let named = Conversation {
            id: "c-1".into(),
            transcript: None,
            prompted: false,
        };
        session.set_hooked_conversation("cursor", named);
        assert_eq!(session.typed_agent, None, "it's the session's own");
    }

    #[test]
    fn an_agent_typed_into_the_shell_is_resumed_by_its_command_while_in_front() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("conv-1.jsonl");
        let conversation = Conversation {
            id: "conv-1".into(),
            transcript: Some(transcript.clone()),
            prompted: false,
        };
        let restart = |front: Option<Front>, reporter: Option<&Reporter>| {
            restart_with(
                Some(conversation.clone()),
                reporter,
                Some("claude"),
                front.as_ref(),
            )
        };
        // Never sent a prompt, it has no transcript to pick up.
        assert_eq!(restart(Some(claude()), None), (None, None));
        std::fs::write(&transcript, "{}\n").unwrap();
        let resume = vec!["claude".to_string(), "--resume".into(), "conv-1".into()];
        assert_eq!(restart(Some(claude()), None), (None, Some(resume)));
        assert_eq!(
            restart(Some(Front::Shell { name: "zsh".into() }), None),
            (None, None)
        );
        assert_eq!(restart(None, None), (None, None));
        // An agent that reports for itself says how it resumes.
        let reporter = Reporter {
            agent: "pi".into(),
            message: None,
            resume: Some(vec!["pi".into(), "--resume".into()]),
            source: None,
        };
        let (_, resume) = restart(Some(claude()), Some(&reporter));
        assert_eq!(resume.unwrap(), ["pi", "--resume"]);
        // crystal's own agent picks its conversation up itself.
        let own = restart_with(Some(conversation.clone()), None, None, None);
        assert_eq!(own, (Some(conversation.clone()), None));
        let codex = restart_with(
            Some(conversation),
            None,
            Some("codex"),
            Some(&Front::Agent {
                program: "codex".into(),
                name: "Codex".into(),
            }),
        );
        assert_eq!(codex.1.unwrap(), ["codex", "resume", "conv-1"]);
    }

    #[test]
    fn subagents_are_counted_as_they_start_and_stop() {
        use AgentEvent::*;
        assert_eq!(subagents_after(0, SubagentStarted), 1);
        assert_eq!(subagents_after(1, SubagentStarted), 2);
        assert_eq!(subagents_after(2, SubagentStopped), 1);
        assert_eq!(subagents_after(0, SubagentStopped), 0, "never below none");
        assert_eq!(subagents_after(2, TurnEnded), 2, "they can outlive a turn");
        assert_eq!(subagents_after(2, Started), 0);
    }

    #[test]
    fn a_subagent_stopping_never_ends_the_turn() {
        assert_eq!(
            after(Some(Working), AgentEvent::SubagentStopped),
            Some(Working)
        );
        assert_eq!(
            after(Some(Waiting), AgentEvent::SubagentStarted),
            Some(Waiting)
        );
        assert_eq!(after(None, AgentEvent::SubagentStarted), None);
    }

    /// A shell's session, running, as a daemon would hand it over, without
    /// a terminal: what it shows is drawn with `show`.
    fn shell_session(dir: &std::path::Path) -> Session {
        let db = crate::db::Db::open(&dir.join("crystal.sock")).unwrap();
        let spending = Arc::new(Spending::new(db));
        let handed = Handed {
            name: "build".into(),
            id: "id-2".into(),
            command: vec!["zsh".into()],
            cwd: dir.to_path_buf(),
            env: BTreeMap::new(),
            pid: None,
            state: State::Running,
            activity: None,
            changed: UNIX_EPOCH,
            looks: Looks::Settled,
            front: Some(Front::Shell { name: "zsh".into() }),
            conversation: None,
            rollouts: None,
            told: None,
            goal: None,
            about: TaskBrief::default(),
            reminded: false,
            reporter: None,
            reporter_job: None,
            named_after_program: false,
            name_given: false,
            title: claude_title::Watch::default(),
            start_from: None,
            screen: vt::Screen::answering(5, 20).save(),
            ended: false,
            stopped_idle: false,
            typed_agent: None,
            subagents: 0,
            model: model::Watch::default(),
            shown: report::Shown::default(),
            pty: None,
            task: None,
        };
        Session::adopt(handed, &spending).unwrap()
    }

    #[test]
    fn a_bell_nobody_watched_marks_the_session_until_it_s_seen() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = shell_session(dir.path());
        session.term().show(b"done\x07\x07");
        session.check();
        assert!(session.info().bell);
        assert_eq!(session.take_changes(), [Change::Bell]);
        // Ringing again while marked is no news.
        session.term().show(b"\x07");
        session.check();
        assert!(session.take_changes().is_empty());
        session.seen();
        assert!(!session.info().bell);
    }

    #[test]
    fn a_copy_nobody_watched_is_counted_and_told_and_one_watched_isn_t() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = shell_session(dir.path());
        // Two in one go are one: only the last would be on the clipboard.
        session.term().show(b"\x1b]52;c;aGk=\x07\x1b]52;c;aGk=\x07");
        session.check();
        assert_eq!(session.info().unseen_copies, 1);
        assert_eq!(session.take_changes(), [Change::UnseenCopy]);
        session.term().show(b"\x1b]52;c;aGk=\x07");
        session.check();
        assert_eq!(session.info().unseen_copies, 2);
        assert_eq!(session.take_changes(), [Change::UnseenCopy]);
        // Looking at the session doesn't take the count back: it only
        // grows, for a TUI to see it go up.
        session.seen();
        assert_eq!(session.info().unseen_copies, 2);

        // A viewer puts what it sees copied on the clipboard itself.
        let watch = session.term().watch(false, false);
        session.term().show(b"\x1b]52;c;aGk=\x07");
        session.term().unwatch(watch.id);
        session.check();
        assert_eq!(session.info().unseen_copies, 2);
        assert!(session.take_changes().is_empty());
    }

    #[test]
    fn a_bell_someone_watches_doesn_t_mark_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = shell_session(dir.path());
        let watch = session.term().watch(false, false);
        session.term().show(b"\x07");
        session.check();
        assert!(!session.info().bell);
        assert!(session.take_changes().is_empty());
        // Nor does one heard while watched, once the viewer has gone.
        session.term().unwatch(watch.id);
        session.check();
        assert!(!session.info().bell);
    }

    fn after(before: Option<Activity>, event: AgentEvent) -> Option<Activity> {
        next_activity(before, event, false)
    }

    #[test]
    fn a_turn_goes_from_working_to_done() {
        assert_eq!(after(None, AgentEvent::Started), Some(Idle));
        assert_eq!(after(Some(Idle), AgentEvent::TurnStarted), Some(Working));
        assert_eq!(after(Some(Working), AgentEvent::TurnEnded), Some(Done));
    }

    #[test]
    fn a_question_waits_until_the_agent_goes_on() {
        assert_eq!(after(Some(Working), AgentEvent::Asking), Some(Waiting));
        assert_eq!(
            after(Some(Waiting), AgentEvent::ToolFinished),
            Some(Working)
        );
    }

    #[test]
    fn a_turn_that_ends_while_watched_is_already_seen() {
        let activity = next_activity(Some(Working), AgentEvent::TurnEnded, true);
        assert_eq!(activity, Some(Idle));
    }

    #[test]
    fn sitting_idle_ends_a_turn_that_never_reported_its_end() {
        assert_eq!(after(Some(Working), AgentEvent::StillIdle), Some(Done));
        assert_eq!(after(Some(Idle), AgentEvent::StillIdle), Some(Idle));
        assert_eq!(after(Some(Waiting), AgentEvent::StillIdle), Some(Waiting));
    }

    #[test]
    fn a_run_s_end_is_taken_only_once_the_session_has_seen_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // A `claude` that answers each prompt at once.
        let claude = dir.path().join("claude");
        let answers = r#"#!/bin/sh
while IFS= read -r line; do
    echo '{"type":"system","subtype":"init","session_id":"c1","model":"m"}'
    echo '{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"c1","total_cost_usd":0.01,"duration_ms":1}'
done
"#;
        std::fs::write(&claude, answers).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let db = crate::db::Db::open(&dir.path().join("crystal.sock")).unwrap();
        let env = BTreeMap::from([("PATH".to_string(), dir.path().display().to_string())]);
        let spec = TaskSpec {
            prompt: "plan it".into(),
            args: Vec::new(),
        };
        let mut session = Session::task(
            "id-1".into(),
            "planner".into(),
            spec,
            Vec::new(),
            dir.path().to_path_buf(),
            env,
            Arc::new(Spending::new(db)),
            None,
        );
        session.prompt("plan it").unwrap();
        // The daemon looks while the run goes on.
        session.check();
        assert_eq!(session.info().activity, Some(Activity::Working));
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.result().is_err() {
            assert!(Instant::now() < deadline, "the run never ended");
            thread::sleep(Duration::from_millis(10));
        }

        // Ended, but not looked at since: as a flow would find it between
        // the daemon's look at its sessions and its look at the flows. Not
        // over for whoever acts on it, who would mark it seen while it still
        // reads as working, for the next look to turn it done.
        assert!(session.finished_run().is_none());
        session.check();
        assert_eq!(session.info().activity, Some(Activity::Done));
        assert_eq!(session.finished_run().unwrap().text, "done");
        session.seen();
        assert_eq!(session.info().activity, Some(Activity::Idle));
        session.stop();
    }

    #[test]
    fn a_task_cancelled_before_its_run_was_looked_at_stays_cancelled() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // A `claude` that works until it's stopped.
        let claude = dir.path().join("claude");
        std::fs::write(&claude, "#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let db = crate::db::Db::open(&dir.path().join("crystal.sock")).unwrap();
        let env = BTreeMap::from([("PATH".to_string(), dir.path().display().to_string())]);
        let spec = TaskSpec {
            prompt: "fix the tests".into(),
            args: Vec::new(),
        };
        let mut session = Session::task(
            "id-1".into(),
            "fixer".into(),
            spec,
            Vec::new(),
            dir.path().to_path_buf(),
            env,
            Arc::new(Spending::new(db)),
            None,
        );
        session.give_task(TaskInfo {
            id: Some(1),
            goal: "fix the tests".into(),
            background: true,
            backlog: None,
            waiting: false,
            created: 1,
            outcome: None,
            brief: Default::default(),
        });
        session.prompt("fix the tests").unwrap();

        // Cancelled and stopped before the daemon looked at the run that
        // started, as a flow's cancel can come.
        let cancelled = session.cancel_task("its flow was cancelled").unwrap();
        assert_eq!(cancelled.state(), TaskState::Cancelled);
        session.stop();
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.is_running() {
            assert!(Instant::now() < deadline, "the task never stopped");
            thread::sleep(Duration::from_millis(20));
        }
        // Looked at since, the run neither opens it again nor has it fail
        // for the session that ended under it.
        session.check();
        assert!(session.take_closed().is_empty());
        let record = session.task_record().unwrap();
        assert_eq!(record.state(), TaskState::Cancelled);
    }
}
