//! What the CLI and the daemon say to each other: one JSON object per line,
//! one request and one response per connection. An attach goes on after its
//! response: the daemon sends the session's output as it comes, and the
//! client sends [`Frame`]s.
//!
//! Every request carries the version of the crystal that sent it. A daemon
//! keeps running the crystal it was started from, even after a new one is
//! installed, and the two may not understand each other: the daemon checks
//! the version before it reads the request, and says what to do.

use crate::events::{Event, Filter, Since};
use crate::flow_run::FlowRun;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, BufRead, ErrorKind, Read, Write};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    New(NewSession),
    /// Start a task: Claude Code run without a terminal.
    NewTask(NewTask),
    List,
    Kill {
        name: String,
    },
    /// Something the agent in a session did, sent by its hooks.
    Report {
        /// The name the session had when its program started, which may
        /// have changed since. `id` finds the session whatever it's called.
        name: String,
        /// The session's id, from programs started since sessions had one.
        #[serde(default)]
        id: Option<String>,
        event: AgentEvent,
        /// The conversation the agent is in, when its hooks say.
        #[serde(default)]
        conversation: Option<Conversation>,
        /// What the user asked, when the event is a prompt sent: the first
        /// names a session crystal named after its program.
        #[serde(default)]
        prompt: Option<String>,
        /// The agent whose hooks sent it, by its program: `claude` or
        /// `codex`. `None` from a crystal that didn't say, which meant
        /// Claude Code.
        #[serde(default)]
        agent: Option<String>,
        /// The directory the agent runs in, as its hooks say.
        #[serde(default)]
        cwd: Option<PathBuf>,
        /// The subagent a subagent's event is about.
        #[serde(default)]
        subagent: Option<Subagent>,
    },
    /// What an agent says about itself with `crystal report`. A program in
    /// a session says which by its `id`; from outside, it's the session's
    /// `name`.
    ReportAgent {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        report: AgentReport,
    },
    /// Tell the user `text` with a notification, the way the daemon tells
    /// them a session needs them: about the session with `id`, or else
    /// called `name`, which a click on it takes them to, when there is one.
    Notify {
        text: String,
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
    /// Give a session another name.
    Rename {
        name: String,
        new_name: String,
    },
    /// Run an ended session's command again, in the same directory and
    /// under the same name, from the client's environment.
    Respawn {
        name: String,
        env: BTreeMap<String, String>,
    },
    /// Type `text` into a session, then press Enter if `enter` is set.
    Send {
        name: String,
        text: String,
        enter: bool,
    },
    /// Press keys in a session: each one a key name, like `Enter` or
    /// `C-c`, or else text typed as it is, never as a paste.
    SendKeys {
        name: String,
        keys: Vec<String>,
    },
    /// A task's answer: what Claude said at the end of its last run.
    Result {
        name: String,
    },
    /// Answer the permission a background task is asking for. `task` is
    /// the task's id or its session's name; `message` is what Claude is
    /// told when it's denied.
    Answer {
        task: String,
        answer: Answer,
        #[serde(default)]
        message: Option<String>,
    },
    /// Stop the run a background task is in the middle of, leaving the
    /// task open.
    Interrupt {
        task: String,
    },
    /// What background tasks have spent today, and the daily budget.
    Spending,
    /// Close a session's task, done or failed. A program in a session says
    /// which by its `id`; from outside, it's the session's `name`.
    Close {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        failed: bool,
        summary: String,
        /// Files in the session's worktree to keep with the task, by their
        /// absolute paths: the daemon checks and copies them itself.
        #[serde(default)]
        artifacts: Vec<PathBuf>,
    },
    /// Add a note to the handoff file of a session's worktree, for the
    /// sessions after it there. `id` and `name` say which session, as for
    /// [`Request::Close`].
    Handoff {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        note: String,
    },
    /// Have the distiller read what the session called `name` did, now,
    /// and keep what a later session would need in its project's memory.
    Distill {
        name: String,
    },
    /// How the model that searches memory by meaning stands.
    EmbeddingStatus,
    /// Get the model that searches memory by meaning ready, in the
    /// background: download it if it isn't here, load it, and give every
    /// entry its vector.
    PrepareEmbeddings,
    /// Search the memory of the project `dir` is in, by words and, with the
    /// model on, by meaning: the daemon keeps the model loaded, once for
    /// every client.
    SearchMemory {
        dir: PathBuf,
        query: String,
        kind: Option<crate::memory::Kind>,
        limit: usize,
    },
    /// The tasks of the project `dir` is in, or of every project with
    /// `all`: those still open, then those waiting to start, then those
    /// closed, the latest first.
    Tasks {
        dir: PathBuf,
        #[serde(default)]
        all: bool,
    },
    /// Make a task that waits to be started: `crystal tasks new
    /// --no-launch`.
    AddTask(PendingTask),
    /// Start the task with this id, which is waiting to, from the client's
    /// environment.
    StartTask {
        id: u64,
        env: BTreeMap<String, String>,
    },
    /// One task, by its id or its session's name.
    ShowTask {
        task: String,
    },
    /// Close a task as cancelled, and stop the session working on it.
    CancelTask {
        task: String,
    },
    /// What happened to a task: how it stands, and its session's
    /// transcript while the session is there.
    TaskLog {
        task: String,
    },
    /// The backlog of the project `dir` is in: what's open, or with `all`,
    /// what's done too.
    BacklogList {
        dir: PathBuf,
        #[serde(default)]
        all: bool,
    },
    /// Put something on the backlog of the project `dir` is in.
    BacklogAdd {
        dir: PathBuf,
        text: String,
        #[serde(default)]
        tags: Vec<String>,
    },
    /// Mark a backlog item done, or open again.
    BacklogMark {
        dir: PathBuf,
        number: u64,
        done: bool,
    },
    /// Take an item off the backlog.
    BacklogRemove {
        dir: PathBuf,
        number: u64,
    },
    /// How many items are open on each of these projects' backlogs, by the
    /// path of the project's main worktree.
    BacklogCounts {
        projects: Vec<PathBuf>,
    },
    /// Something that happened outside the daemon, like a worktree a
    /// client made or an entry it added to memory: the daemon numbers it,
    /// writes it in the event log, and passes it on to whoever listens.
    Emit {
        event: Box<Event>,
    },
    /// Turn the connection into a stream of the events `filter` takes, one
    /// JSON line each, after [`Response::Subscribed`]. With `since`, the
    /// events the log has from then come first, so a client can catch up
    /// without a gap.
    Subscribe {
        #[serde(default)]
        filter: Filter,
        #[serde(default)]
        since: Option<Since>,
    },
    /// Wait until a line on a session's screen, or just scrolled off it,
    /// matches the regular expression `pattern`, or `timeout_ms` has
    /// passed.
    WaitOutput {
        name: String,
        pattern: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Start a run of the flow called `flow` on `goal`, from `cwd`. Its
    /// steps' tasks start from the client's environment, `env`.
    StartFlow {
        flow: String,
        goal: String,
        cwd: PathBuf,
        env: BTreeMap<String, String>,
    },
    /// Every flow run the daemon knows of, the oldest first.
    ListFlows,
    /// Go on past the gate the run called `run` waits at.
    ApproveFlow {
        run: String,
    },
    /// Send the run called `run` back from the gate it waits at, with
    /// notes on what to do differently.
    SendFlowBack {
        run: String,
        notes: String,
    },
    /// Run the step that stopped the run called `run` again: it failed, or
    /// a restart cut it short.
    RetryFlow {
        run: String,
    },
    /// Cancel the run called `run`: its step's open task is cancelled and
    /// its session stopped, and the run goes no further.
    CancelFlow {
        run: String,
    },
    /// Carry out a layout command in the TUI used last, and answer with the
    /// layout it comes to: `crystal tab`, `crystal pane` and `crystal
    /// layout`.
    Layout(crate::layout::Order),
    /// A TUI offers to carry out layout commands, saying when it was last
    /// used, in milliseconds since the Unix epoch. After
    /// [`Response::Done`], the daemon writes it each one as a
    /// [`crate::layout::Relayed`] line, and it writes back
    /// [`crate::layout::Report`] lines. A handover cuts it, and the TUI
    /// offers again.
    TakeLayoutOrders {
        used: u64,
    },
    /// What's on a session's screen, as text.
    Read {
        name: String,
        /// The rows that have scrolled up off the screen too, ahead of it.
        #[serde(default)]
        history: bool,
    },
    /// With no name, the newest session.
    Attach {
        name: Option<String>,
        rows: u16,
        cols: u16,
        /// Send the session's history ahead of its screen, so the viewer
        /// can scroll back through it.
        #[serde(default)]
        history: bool,
    },
    /// Stop the daemon. With `keep_sessions`, the running sessions stay
    /// written down, so that the next daemon starts them again.
    ///
    /// Every version must keep this one as it is, and every daemon takes it
    /// whatever the version of the crystal sending it: stopping the daemon
    /// is the way out when the two don't match.
    Shutdown {
        #[serde(default)]
        keep_sessions: bool,
    },
    /// Hand the daemon over to the crystal at `exe`, which carries on in its
    /// place, its sessions running on (see [`crate::handover`]). `format`
    /// is the kind of handover that crystal reads: a daemon that writes
    /// another refuses, and is restarted cold.
    ///
    /// Like a shutdown, every version must keep this one as it is, and
    /// every daemon takes it whatever the version of the crystal sending
    /// it: handing over is how a daemon becomes another version.
    Handover {
        exe: PathBuf,
        format: u32,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NewSession {
    /// `None` names the session for its task, or else after its program
    /// until its first prompt names it.
    pub name: Option<String>,
    pub cwd: PathBuf,
    pub command: Vec<String>,
    /// The client's environment, which the program starts from.
    pub env: BTreeMap<String, String>,
    /// What the agent is asked to do, which makes the session a task. It's
    /// in `command` already, as the agent's first prompt; this says which
    /// part of the command it is.
    #[serde(default)]
    pub task: Option<String>,
    /// The backlog item the task is for, which closing it done ticks.
    #[serde(default)]
    pub backlog: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NewTask {
    /// `None` names the task for its prompt, or else "task", with a number
    /// added if that's taken.
    pub name: Option<String>,
    pub cwd: PathBuf,
    pub spec: TaskSpec,
    /// The client's environment, which Claude starts from.
    pub env: BTreeMap<String, String>,
    /// The backlog item the task is for, which closing it done ticks.
    #[serde(default)]
    pub backlog: Option<u64>,
}

/// A task made to start later: what it's to do, where, and how it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTask {
    /// Given by the daemon as it takes the task.
    #[serde(default)]
    pub id: u64,
    pub goal: String,
    pub cwd: PathBuf,
    /// The session's name, once it starts: `None` names it for its goal,
    /// or else after its program.
    #[serde(default)]
    pub name: Option<String>,
    pub start: TaskStart,
    #[serde(default)]
    pub backlog: Option<u64>,
    /// When it was made, in seconds since the Unix epoch.
    #[serde(default)]
    pub created: u64,
}

/// How a task starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskStart {
    /// An agent in a terminal: this command, with the goal in it as the
    /// agent's first prompt.
    Agent { command: Vec<String> },
    /// In the background, `claude -p` with these arguments.
    Background { args: Vec<String> },
}

/// An answer to the permission a background task asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// Yes, this once.
    Allow,
    Deny,
    /// Yes, and a rule for calls like it, so they're not asked about again.
    Always,
}

/// What a task is asked to do: the prompt it starts with, and arguments
/// for each `claude -p` it runs, like `--permission-mode acceptEdits`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub prompt: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// What `crystal result` answers with: a task's last answer, and what the
/// task has come to so far.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskResult {
    /// What Claude said at the end of its last run, or what went wrong.
    pub text: String,
    /// Whether the last run failed.
    pub failed: bool,
    /// The conversation's id, which `claude --resume` takes.
    pub conversation: Option<String>,
    /// What the task has cost so far, in US dollars, as Claude counts it.
    pub cost_usd: f64,
    /// How many times Claude has run for it: its prompt, then each
    /// follow-up.
    pub runs: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Created {
        name: String,
        /// The id of the task the session was started with, if it was.
        #[serde(default)]
        task: Option<u64>,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    /// `running` is false when the session has already ended: the daemon
    /// sends its last screen and hangs up.
    Attached {
        name: String,
        #[serde(default)]
        id: String,
        running: bool,
    },
    /// A session's screen, one string per row.
    Screen {
        rows: Vec<String>,
    },
    /// A task's answer, and what it has come to.
    Result(TaskResult),
    /// Tasks, as `Request::Tasks` asks for them.
    Tasks {
        tasks: Vec<TaskView>,
    },
    /// The id a new task got.
    TaskAdded {
        id: u64,
    },
    /// One task, as `Request::ShowTask` asks for it.
    Task(TaskView),
    /// What happened to a task: how it stands, and its session's
    /// transcript, while the session is still there.
    TaskLog {
        task: TaskView,
        transcript: Option<Vec<String>>,
    },
    Spending(Spending),
    /// A project's backlog.
    Backlog(Backlog),
    /// The number a new backlog item got.
    Added {
        number: u64,
    },
    /// How many items are open on each project's backlog.
    BacklogCounts {
        open: BTreeMap<PathBuf, usize>,
    },
    /// The name a new flow run got.
    FlowStarted {
        run: String,
    },
    /// Flow runs, as `Request::ListFlows` asks for them.
    Flows {
        runs: Vec<FlowRun>,
    },
    /// What came of the distiller reading what a session did.
    Distilled(crate::distill::Report),
    /// How the model that searches memory by meaning stands.
    EmbeddingStatus(crate::embed::Status),
    /// The entries a memory search found, the best first.
    Memory {
        entries: Vec<crate::memory::Listed>,
    },
    /// What the hook that reported a turn ending tells its agent: its task
    /// is still open. The agent carries on, so the turn hasn't ended.
    Remind {
        text: String,
    },
    /// The stream of events has started: the ones from the log come
    /// first, those up to `seq`, then each new one as it happens.
    Subscribed {
        seq: u64,
    },
    /// The line on a session's screen that matched.
    Matched {
        line: String,
    },
    /// The daemon a handover was asked of has been handed over: it runs the
    /// new crystal now, which says so, with how many sessions carried on.
    HandedOver {
        sessions: usize,
    },
    /// A TUI's tabs and their panes.
    Layout(crate::layout::Layout),
    Done,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    /// Unlike its name, a session's id never changes: it's how a program
    /// tells which session it runs in, whatever the session is called now.
    #[serde(default)]
    pub id: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub pid: Option<u32>,
    pub state: State,
    /// `None` for a program that doesn't report what it's doing.
    pub activity: Option<Activity>,
    /// `None` when the session's directory isn't in a git repository.
    pub worktree: Option<Worktree>,
    /// When the session last changed, in seconds since the Unix epoch: it
    /// started, its agent started or stopped doing something, or its
    /// program ended. 0 from a daemon that doesn't say.
    #[serde(default)]
    pub changed: u64,
    /// What's in front in the session's terminal, which can change as it
    /// runs: a shell can have an agent in front, and the shell back.
    /// `None` until it's been looked at, and from a daemon that doesn't
    /// say.
    #[serde(default)]
    pub front: Option<Front>,
    /// What the session's agent was asked to do, when it was, and how that
    /// went.
    #[serde(default)]
    pub task: Option<TaskInfo>,
    /// The permission a background task is waiting on the user for.
    #[serde(default)]
    pub asking: Option<Asking>,
    /// The agent that says what it's doing itself, with `crystal report`,
    /// while it holds the session.
    #[serde(default)]
    pub reporter: Option<Reporter>,
    /// How many subagents its agent has running, as its hooks say.
    #[serde(default)]
    pub subagents: u32,
}

/// A subagent an agent started, as its hooks name it: its id, and its
/// type, like `Explore`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subagent {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

/// An agent that says what it's doing itself, with `crystal report`, and
/// how to pick its session up again after a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reporter {
    /// The name it gave, which is what's in front in the session.
    pub agent: String,
    /// What it said with its last report, like what it waits on the user
    /// for.
    #[serde(default)]
    pub message: Option<String>,
    /// The command that picks its session up again after a restart.
    #[serde(default)]
    pub resume: Option<Vec<String>>,
}

/// What an agent says about itself with `crystal report`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentReport {
    /// It's doing `state`, which takes the session's status over from
    /// crystal's own reading of it. `None` for the agent keeps the name it
    /// gave before; `resume`, the command that picks its session up again.
    State {
        #[serde(default)]
        agent: Option<String>,
        state: Activity,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        resume: Option<Vec<String>>,
    },
    /// Only the command that picks its session up again, from an agent
    /// that holds the session.
    Resume {
        #[serde(default)]
        agent: Option<String>,
        argv: Vec<String>,
    },
    /// It lets go of the session: crystal reads what the session does for
    /// itself again, and forgets the agent's name and command.
    Release,
}

/// A permission a background task's Claude asks for: the tool, and what
/// it's asked to do with it, like `Bash` and `cargo test`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asking {
    pub tool: String,
    pub gist: String,
}

/// What background tasks have spent today, by Claude's own count, and the
/// daily budget, if there is one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Spending {
    pub today_usd: f64,
    /// 0 for none.
    pub daily_budget_usd: f64,
}

impl Spending {
    pub fn over_budget(&self) -> bool {
        self.daily_budget_usd > 0.0 && self.today_usd >= self.daily_budget_usd
    }
}

impl SessionInfo {
    /// The one word `ls` shows for it: what its agent is doing, when it
    /// runs one that says, or else whether it's running or how it ended.
    pub fn status(&self) -> String {
        match (&self.state, self.activity) {
            (State::Running, Some(activity)) => activity.to_string(),
            (state, _) => state.to_string(),
        }
    }
}

/// What's in front in a session's terminal: the program its keys go to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Front {
    /// A coding agent crystal knows: its program, like `claude`, and what
    /// it's called, like `Claude Code`.
    Agent {
        program: String,
        name: String,
    },
    Shell {
        name: String,
    },
    /// Any other program, by its name.
    Program {
        name: String,
    },
    /// A task: Claude Code run without a terminal.
    Task,
}

impl Front {
    /// One word for it, the way `ls` shows it: `claude`, `zsh`, `vite`.
    pub fn word(&self) -> &str {
        match self {
            Front::Agent { program, .. } => program,
            Front::Shell { name } | Front::Program { name } => name,
            Front::Task => "task",
        }
    }

    pub fn is_agent(&self) -> bool {
        matches!(self, Front::Agent { .. })
    }
}

/// A session's task: what its agent was asked to do, and, once it's
/// closed, how that went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskInfo {
    /// Its number, which `crystal tasks` shows as `t12`. `None` for a task
    /// from before tasks had one.
    #[serde(default)]
    pub id: Option<u64>,
    /// What the agent was asked to do.
    pub goal: String,
    /// Whether it runs in the background, without a terminal.
    #[serde(default)]
    pub background: bool,
    /// The backlog item it's for, which closing it done ticks.
    #[serde(default)]
    pub backlog: Option<u64>,
    /// Its agent's turn ended with the task still open: it's asking the
    /// user something.
    #[serde(default)]
    pub waiting: bool,
    /// When it was made, in seconds since the Unix epoch.
    #[serde(default)]
    pub created: u64,
    /// `None` while the task is open.
    #[serde(default)]
    pub outcome: Option<TaskOutcome>,
}

impl TaskInfo {
    pub fn state(&self) -> TaskState {
        match &self.outcome {
            Some(outcome) => outcome.state(),
            None if self.waiting => TaskState::Waiting,
            None => TaskState::Running,
        }
    }

    pub fn is_open(&self) -> bool {
        self.outcome.is_none()
    }
}

/// How a task went, once it's closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskOutcome {
    pub failed: bool,
    /// The user cancelled it, or killed its session while it was open.
    #[serde(default)]
    pub cancelled: bool,
    /// A line on what was done, or why it couldn't be.
    pub summary: String,
    /// When it was closed, in seconds since the Unix epoch.
    pub closed: u64,
}

impl TaskOutcome {
    /// An outcome for a task that has just come to `state`: done, failed
    /// or cancelled.
    pub fn new(state: TaskState, summary: &str, closed: u64) -> TaskOutcome {
        TaskOutcome {
            failed: state == TaskState::Failed,
            cancelled: state == TaskState::Cancelled,
            summary: summary.trim().to_string(),
            closed,
        }
    }

    pub fn state(&self) -> TaskState {
        if self.cancelled {
            TaskState::Cancelled
        } else if self.failed {
            TaskState::Failed
        } else {
            TaskState::Done
        }
    }
}

/// How a task stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Made to start later, with `--no-launch`: nothing works on it yet.
    Pending,
    /// Its session is working on it.
    Running,
    /// Its agent's turn ended with it still open: it's asking the user
    /// something.
    Waiting,
    Done,
    Failed,
    /// The user cancelled it, or killed its session while it was open.
    Cancelled,
}

impl TaskState {
    pub fn word(self) -> &'static str {
        match self {
            TaskState::Pending => "pending",
            TaskState::Running => "running",
            TaskState::Waiting => "waiting",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
        }
    }
}

/// A task as `crystal tasks` lists it: one still open in a session, one
/// waiting to start, or one closed, from its project's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    #[serde(default)]
    pub id: Option<u64>,
    pub goal: String,
    /// The session it ran in, under the name it had then. Empty for a task
    /// that hasn't started.
    pub session: String,
    pub project: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub background: bool,
    #[serde(default)]
    pub backlog: Option<u64>,
    /// Made with `--no-launch`, and not started yet.
    #[serde(default)]
    pub pending: bool,
    #[serde(default)]
    pub waiting: bool,
    #[serde(default)]
    pub created: u64,
    /// `None` while it's open.
    #[serde(default)]
    pub outcome: Option<TaskOutcome>,
    /// The files kept with it as it closed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
}

impl TaskRecord {
    pub fn state(&self) -> TaskState {
        match &self.outcome {
            Some(outcome) => outcome.state(),
            None if self.pending => TaskState::Pending,
            None if self.waiting => TaskState::Waiting,
            None => TaskState::Running,
        }
    }
}

/// A task as the CLI shows it: what's kept of it, how it stands, and, while
/// a session works on it, what that session is doing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskView {
    #[serde(flatten)]
    pub record: TaskRecord,
    pub state: TaskState,
    /// How its session stands, while the session is still there.
    #[serde(default)]
    pub session_state: Option<State>,
    /// The permission a background task is waiting on the user for.
    #[serde(default)]
    pub asking: Option<Asking>,
    /// What a background task has cost so far, in US dollars.
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

impl TaskView {
    /// A task that no session is working on now.
    pub fn of_record(record: TaskRecord) -> TaskView {
        TaskView {
            state: record.state(),
            record,
            session_state: None,
            asking: None,
            cost_usd: None,
        }
    }
}

/// A file kept with a task as it closed: copied out of its worktree into
/// crystal's state directory, so it outlives the worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub kind: ArtifactKind,
    /// The copy's name, which is the file's own unless two had one name.
    pub name: String,
    /// Where the copy is.
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A file `crystal done --artifact` named.
    File,
    /// The worktree's handoff file as it was when the task closed.
    Handoff,
}

impl ArtifactKind {
    pub fn word(self) -> &'static str {
        match self {
            ArtifactKind::File => "file",
            ArtifactKind::Handoff => "handoff",
        }
    }

    /// The kind called `word`, as [`ArtifactKind::word`] says it.
    pub fn named(word: &str) -> Option<ArtifactKind> {
        [ArtifactKind::File, ArtifactKind::Handoff]
            .into_iter()
            .find(|kind| kind.word() == word)
    }
}

/// How a task is shown: `t12`.
pub fn task_label(id: Option<u64>) -> String {
    match id {
        Some(id) => format!("t{id}"),
        None => "-".to_string(),
    }
}

/// One project's backlog: things to do later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backlog {
    /// The project's name, as the sidebar shows it.
    pub project: String,
    /// The project's main worktree, or the directory itself outside git.
    pub path: PathBuf,
    pub items: Vec<BacklogItem>,
}

/// A thing to do later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacklogItem {
    /// Its number in the project's backlog, which never changes: #1, #2…
    pub number: u64,
    pub text: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub done: bool,
    /// When it was put on the backlog, in seconds since the Unix epoch.
    pub created: u64,
    /// When it was done, while it is.
    #[serde(default)]
    pub closed: Option<u64>,
}

/// The git worktree a session runs in, and the project it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// The project's name: the name of its main worktree's directory.
    pub project: String,
    /// The main worktree's directory, which tells projects apart.
    pub project_path: PathBuf,
    /// This worktree's top directory.
    pub path: PathBuf,
    /// Whether this is the repository's main worktree, rather than one
    /// linked to it with `git worktree add`.
    pub main: bool,
    /// The branch checked out, or `None` when HEAD is detached.
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    Exited { code: u32 },
    Signaled { signal: String },
}

/// An agent's conversation, as its hooks name it: what it takes to pick
/// the conversation up again after a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    /// The file the agent keeps the conversation in.
    pub transcript: Option<PathBuf>,
}

impl Conversation {
    /// Whether there's anything to pick up: an agent that was never sent a
    /// prompt hasn't written its transcript, and can't resume it. Codex
    /// compresses the transcripts it hasn't touched in a while, adding
    /// `.zst` to the name, and resumes those all the same.
    pub fn can_resume(&self) -> bool {
        let Some(path) = &self.transcript else {
            return false;
        };
        let compressed = PathBuf::from(format!("{}.zst", path.display()));
        path.is_file() || compressed.is_file()
    }
}

/// What an agent's hooks report, in terms that fit any agent. The daemon
/// works out the session's [`Activity`] from these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEvent {
    /// The agent is up and waiting for its first prompt.
    Started,
    TurnStarted,
    /// A tool call finished, so the agent is back to work, say after the
    /// user allowed it.
    ToolFinished,
    /// The agent is asking the user something: a permission, a question.
    Asking,
    TurnEnded,
    /// The agent has sat at its prompt for a while.
    StillIdle,
    /// The agent started a subagent, which says nothing about what the
    /// agent itself is doing.
    SubagentStarted,
    /// One of its subagents finished. The agent's turn goes on.
    SubagentStopped,
}

/// What the agent in a session is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    /// Working on a turn.
    Working,
    /// Stopped until the user answers it, say a permission prompt.
    Waiting,
    /// Finished its turn, and nobody has looked at it since.
    Done,
    /// Finished its turn, and it has been seen.
    Idle,
}

impl fmt::Display for Activity {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = match self {
            Activity::Working => "working",
            Activity::Waiting => "waiting",
            Activity::Done => "done",
            Activity::Idle => "idle",
        };
        f.write_str(name)
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            State::Running => write!(f, "running"),
            State::Exited { code } => write!(f, "exited {code}"),
            State::Signaled { signal } => write!(f, "killed ({signal})"),
        }
    }
}

/// The version of this crystal, which every request carries.
pub fn version() -> String {
    // Lets the tests play a crystal of another version against this one.
    // Only debug builds listen, so a release always tells the truth.
    if cfg!(debug_assertions)
        && let Ok(pretend) = std::env::var("CRYSTAL_PRETEND_VERSION")
    {
        return pretend;
    }
    env!("CARGO_PKG_VERSION").to_string()
}

/// Sends a request, with this crystal's version beside its own fields. A
/// daemon from before versions were sent ignores the extra field, as it
/// does any field it doesn't know.
pub fn send_request(out: impl Write, request: &Request) -> io::Result<()> {
    let mut message = serde_json::to_value(request)?;
    message["version"] = Value::String(version());
    send(out, &message)
}

/// A request as it arrives: the version of the crystal that sent it, and
/// the request itself, not read yet. A crystal of another version may send
/// a request this one has never heard of.
pub struct Incoming {
    /// `None` from a crystal that didn't say.
    pub version: Option<String>,
    message: Value,
}

impl Incoming {
    pub fn is_shutdown(&self) -> bool {
        self.message["type"] == "shutdown"
    }

    pub fn is_handover(&self) -> bool {
        self.message["type"] == "handover"
    }

    /// Reads the request, once its version is known to match.
    pub fn request(self) -> serde_json::Result<Request> {
        serde_json::from_value(self.message)
    }
}

/// The next request, or `None` once the client has hung up.
pub fn recv_request(input: impl BufRead) -> io::Result<Option<Incoming>> {
    let Some(mut message) = recv::<Value>(input)? else {
        return Ok(None);
    };
    let version = match message
        .as_object_mut()
        .and_then(|fields| fields.remove("version"))
    {
        Some(Value::String(version)) => Some(version),
        _ => None,
    };
    Ok(Some(Incoming { version, message }))
}

pub fn send<T: Serialize>(mut out: impl Write, message: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.flush()
}

/// The next message, or `None` once the other side has hung up.
pub fn recv<T: DeserializeOwned>(mut input: impl BufRead) -> io::Result<Option<T>> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}

/// What an attached client sends: keys for the session, and its size.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Input(Vec<u8>),
    Resize { rows: u16, cols: u16 },
}

const INPUT: u8 = 0;
const RESIZE: u8 = 1;

pub fn send_frame(mut out: impl Write, frame: &Frame) -> io::Result<()> {
    let mut bytes = Vec::new();
    match frame {
        Frame::Input(input) => {
            bytes.push(INPUT);
            bytes.extend_from_slice(&(input.len() as u32).to_be_bytes());
            bytes.extend_from_slice(input);
        }
        Frame::Resize { rows, cols } => {
            bytes.push(RESIZE);
            bytes.extend_from_slice(&rows.to_be_bytes());
            bytes.extend_from_slice(&cols.to_be_bytes());
        }
    }
    out.write_all(&bytes)
}

/// The next frame, or `None` once the client has hung up.
pub fn recv_frame(mut input: impl Read) -> io::Result<Option<Frame>> {
    let mut kind = [0];
    if input.read(&mut kind)? == 0 {
        return Ok(None);
    }
    match kind[0] {
        INPUT => {
            let mut len = [0; 4];
            input.read_exact(&mut len)?;
            let mut bytes = vec![0; u32::from_be_bytes(len) as usize];
            input.read_exact(&mut bytes)?;
            Ok(Some(Frame::Input(bytes)))
        }
        RESIZE => {
            let mut size = [0; 4];
            input.read_exact(&mut size)?;
            Ok(Some(Frame::Resize {
                rows: u16::from_be_bytes([size[0], size[1]]),
                cols: u16::from_be_bytes([size[2], size[3]]),
            }))
        }
        kind => Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("unknown frame kind {kind}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversation_resumes_only_once_its_transcript_exists() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("abc.jsonl");
        let conversation = Conversation {
            id: "abc".into(),
            transcript: Some(transcript.clone()),
        };
        assert!(!conversation.can_resume());
        std::fs::write(&transcript, "{}\n").unwrap();
        assert!(conversation.can_resume());
    }

    #[test]
    fn frames_survive_the_round_trip() {
        let frames = [
            Frame::Input(b"hello\r".to_vec()),
            Frame::Resize {
                rows: 50,
                cols: 200,
            },
            Frame::Input(Vec::new()),
        ];
        let mut wire = Vec::new();
        for frame in &frames {
            send_frame(&mut wire, frame).unwrap();
        }

        let mut wire = &wire[..];
        for frame in frames {
            assert_eq!(recv_frame(&mut wire).unwrap(), Some(frame));
        }
        assert_eq!(recv_frame(&mut wire).unwrap(), None);
    }

    #[test]
    fn a_request_survives_the_round_trip() {
        let mut wire = Vec::new();
        let request = Request::Kill {
            name: "claude".into(),
        };
        send(&mut wire, &request).unwrap();
        assert_eq!(wire.last(), Some(&b'\n'));

        let back: Request = recv(&wire[..]).unwrap().unwrap();
        assert!(matches!(back, Request::Kill { name } if name == "claude"));
    }

    #[test]
    fn a_request_carries_the_version_that_sent_it() {
        let mut wire = Vec::new();
        send_request(&mut wire, &Request::List).unwrap();

        let incoming = recv_request(&wire[..]).unwrap().unwrap();
        assert_eq!(incoming.version, Some(version()));
        assert!(matches!(incoming.request().unwrap(), Request::List));
    }

    #[test]
    fn a_request_from_a_crystal_that_doesnt_say_has_no_version() {
        let incoming = recv_request(&br#"{"type":"list"}"#[..]).unwrap().unwrap();
        assert_eq!(incoming.version, None);
    }

    #[test]
    fn a_request_unknown_here_still_has_its_version_read() {
        let line = br#"{"type":"teleport","version":"9.0.0"}"#;
        let incoming = recv_request(&line[..]).unwrap().unwrap();
        assert_eq!(incoming.version.as_deref(), Some("9.0.0"));
        assert!(incoming.request().is_err());
    }

    #[test]
    fn a_shutdown_is_known_as_one_whatever_the_version() {
        let mut wire = Vec::new();
        let shutdown = Request::Shutdown {
            keep_sessions: true,
        };
        send_request(&mut wire, &shutdown).unwrap();
        assert!(recv_request(&wire[..]).unwrap().unwrap().is_shutdown());
    }

    #[test]
    fn a_handover_is_known_as_one_whatever_the_version() {
        let line = br#"{"type":"handover","exe":"/bin/crystal","format":1,"version":"9.0.0"}"#;
        let incoming = recv_request(&line[..]).unwrap().unwrap();
        assert!(incoming.is_handover());
        assert!(!incoming.is_shutdown());
        let Request::Handover { exe, format } = incoming.request().unwrap() else {
            panic!("not a handover");
        };
        assert_eq!((exe, format), (PathBuf::from("/bin/crystal"), 1));
    }

    #[test]
    fn recv_reports_a_hang_up_as_none() {
        let back: Option<Request> = recv(&b""[..]).unwrap();
        assert!(back.is_none());
    }
}
