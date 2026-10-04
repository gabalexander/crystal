//! What happens in crystal, as events: the one [`Event`] the daemon writes
//! in its log, streams to the clients that subscribe and hands to plugins'
//! `[[events]]` hooks, so that a script, an agent and a plugin all see the
//! same thing. [`crate::event_log`] keeps the log and does the handing on.
//!
//! An event is one JSON object: its `seq` in the log, when it happened
//! (`at`), what happened (`event`, one of [`Kind`]'s names), the `project`
//! and `session` it's about, and whatever its kind carries besides, like the
//! `task` that closed. Plugins listen for events by name, so a name, once
//! given, stays.

use crate::artifacts;
use crate::flow_run::{FlowRun, StepState};
use crate::memory::{self, Entry};
use crate::messages::{self, Sender};
use crate::plugin_manifest;
use crate::project;
use crate::protocol::{
    Activity, Answer, Artifact, ArtifactKind, Asking, BacklogItem, Reporter, SessionInfo, State,
    Subagent, TaskOutcome, TaskRecord, TaskResult, TaskState,
};
use crate::shell;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// What happened. A kind can be added, but never renamed or taken away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "&'static str", try_from = "String")]
pub enum Kind {
    SessionStarted,
    SessionRenamed,
    SessionWorking,
    SessionWaiting,
    SessionDone,
    SessionIdle,
    SessionEnded,
    SessionStartFailed,
    SessionRemoved,
    SessionArchived,
    SessionUnarchived,
    SessionOpenedInTerminal,
    SessionClaimed,
    SessionReleased,
    SubagentStarted,
    SubagentStopped,
    SessionMessage,
    SessionBell,
    SessionCopyDropped,
    TaskOpened,
    TaskStarted,
    TaskWaiting,
    TaskReminded,
    TaskClosed,
    TaskArtifact,
    RunStarted,
    RunToolUse,
    RunAsking,
    RunAnswered,
    RunInterrupted,
    RunEnded,
    FlowStarted,
    FlowStepStarted,
    FlowStepEnded,
    FlowGate,
    FlowGateAnswered,
    FlowEnded,
    WorktreeCreated,
    WorktreeRemoved,
    WorktreeHookFailed,
    HandoffAdded,
    MemoryAdded,
    MemoryForgotten,
    MemoryStale,
    MemoryPromoted,
    MemoryDistilled,
    MemoryDistillFailed,
    BacklogAdded,
    BacklogClosed,
    PluginPaused,
    DaemonHandedOver,
    DaemonRestarted,
}

impl Kind {
    pub const ALL: [Kind; 52] = [
        Kind::SessionStarted,
        Kind::SessionRenamed,
        Kind::SessionWorking,
        Kind::SessionWaiting,
        Kind::SessionDone,
        Kind::SessionIdle,
        Kind::SessionEnded,
        Kind::SessionStartFailed,
        Kind::SessionRemoved,
        Kind::SessionArchived,
        Kind::SessionUnarchived,
        Kind::SessionOpenedInTerminal,
        Kind::SessionClaimed,
        Kind::SessionReleased,
        Kind::SubagentStarted,
        Kind::SubagentStopped,
        Kind::SessionMessage,
        Kind::SessionBell,
        Kind::SessionCopyDropped,
        Kind::TaskOpened,
        Kind::TaskStarted,
        Kind::TaskWaiting,
        Kind::TaskReminded,
        Kind::TaskClosed,
        Kind::TaskArtifact,
        Kind::RunStarted,
        Kind::RunToolUse,
        Kind::RunAsking,
        Kind::RunAnswered,
        Kind::RunInterrupted,
        Kind::RunEnded,
        Kind::FlowStarted,
        Kind::FlowStepStarted,
        Kind::FlowStepEnded,
        Kind::FlowGate,
        Kind::FlowGateAnswered,
        Kind::FlowEnded,
        Kind::WorktreeCreated,
        Kind::WorktreeRemoved,
        Kind::WorktreeHookFailed,
        Kind::HandoffAdded,
        Kind::MemoryAdded,
        Kind::MemoryForgotten,
        Kind::MemoryStale,
        Kind::MemoryPromoted,
        Kind::MemoryDistilled,
        Kind::MemoryDistillFailed,
        Kind::BacklogAdded,
        Kind::BacklogClosed,
        Kind::PluginPaused,
        Kind::DaemonHandedOver,
        Kind::DaemonRestarted,
    ];

    /// Its name, which is how plugins, filters and the log know it.
    pub fn name(self) -> &'static str {
        match self {
            Kind::SessionStarted => "session.started",
            Kind::SessionRenamed => "session.renamed",
            Kind::SessionWorking => "session.working",
            Kind::SessionWaiting => "session.waiting",
            Kind::SessionDone => "session.done",
            Kind::SessionIdle => "session.idle",
            Kind::SessionEnded => "session.ended",
            Kind::SessionStartFailed => "session.start_failed",
            Kind::SessionRemoved => "session.removed",
            Kind::SessionArchived => "session.archived",
            Kind::SessionUnarchived => "session.unarchived",
            Kind::SessionOpenedInTerminal => "session.opened_in_terminal",
            Kind::SessionClaimed => "session.claimed",
            Kind::SessionReleased => "session.released",
            Kind::SubagentStarted => "subagent.started",
            Kind::SubagentStopped => "subagent.stopped",
            Kind::SessionMessage => "session.message",
            Kind::SessionBell => "session.bell",
            Kind::SessionCopyDropped => "session.copy_dropped",
            Kind::TaskOpened => "task.opened",
            Kind::TaskStarted => "task.started",
            Kind::TaskWaiting => "task.waiting",
            Kind::TaskReminded => "task.reminded",
            Kind::TaskClosed => "task.closed",
            Kind::TaskArtifact => "task.artifact",
            Kind::RunStarted => "run.started",
            Kind::RunToolUse => "run.tool_use",
            Kind::RunAsking => "run.asking",
            Kind::RunAnswered => "run.answered",
            Kind::RunInterrupted => "run.interrupted",
            Kind::RunEnded => "run.ended",
            Kind::FlowStarted => "flow.started",
            Kind::FlowStepStarted => "flow.step_started",
            Kind::FlowStepEnded => "flow.step_ended",
            Kind::FlowGate => "flow.gate",
            Kind::FlowGateAnswered => "flow.gate_answered",
            Kind::FlowEnded => "flow.ended",
            Kind::WorktreeCreated => "worktree.created",
            Kind::WorktreeRemoved => "worktree.removed",
            Kind::WorktreeHookFailed => "worktree.hook_failed",
            Kind::HandoffAdded => "handoff.added",
            Kind::MemoryAdded => "memory.added",
            Kind::MemoryForgotten => "memory.forgotten",
            Kind::MemoryStale => "memory.stale",
            Kind::MemoryPromoted => "memory.promoted",
            Kind::MemoryDistilled => "memory.distilled",
            Kind::MemoryDistillFailed => "memory.distill_failed",
            Kind::BacklogAdded => "backlog.added",
            Kind::BacklogClosed => "backlog.closed",
            Kind::PluginPaused => "plugin.paused",
            Kind::DaemonHandedOver => "daemon.handed_over",
            Kind::DaemonRestarted => "daemon.restarted",
        }
    }

    pub fn named(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// When it happens, in a line: what `crystal plugin events` says of it.
    pub fn about(self) -> &'static str {
        match self {
            Kind::SessionStarted => "a session starts, or starts again",
            Kind::SessionRenamed => "a session gets another name",
            Kind::SessionWorking => "a session's agent starts working on a turn",
            Kind::SessionWaiting => "a session's agent comes to wait on you",
            Kind::SessionDone => "a session's agent finishes a turn nobody was watching",
            Kind::SessionIdle => "a session's agent is at its prompt, its turn seen",
            Kind::SessionEnded => "a session's program ends, or the session is killed",
            Kind::SessionStartFailed => "a session can't start again after a restart",
            Kind::SessionRemoved => "a session leaves the list: killed, or its worktree removed",
            Kind::SessionArchived => "a session is stopped and kept in the archive",
            Kind::SessionUnarchived => "a session is started again from the archive",
            Kind::SessionOpenedInTerminal => "a background task is opened in a terminal",
            Kind::SessionClaimed => "an agent takes over saying what a session is doing",
            Kind::SessionReleased => "that agent lets go of the session",
            Kind::SubagentStarted => "a session's agent starts a subagent",
            Kind::SubagentStopped => "that subagent finishes",
            Kind::SessionMessage => "a session is sent a message: by another session, or by you",
            Kind::SessionBell => "a session's program rings the bell while nobody's watching",
            Kind::SessionCopyDropped => "a session's program copies while nobody's watching",
            Kind::TaskOpened => "a task is made, or opened again by a follow-up",
            Kind::TaskStarted => "a task made to start later starts",
            Kind::TaskWaiting => "a task's agent ends a turn with the task still open",
            Kind::TaskReminded => "an agent ending its turn is reminded to close its task",
            Kind::TaskClosed => "a task closes, done, failed or cancelled",
            Kind::TaskArtifact => "a file is kept with a task as it closes",
            Kind::RunStarted => "a background task starts a run of Claude",
            Kind::RunToolUse => "a background task's Claude uses a tool",
            Kind::RunAsking => "a background task's Claude asks you for a permission",
            Kind::RunAnswered => "you answer it",
            Kind::RunInterrupted => "you stop a background task's run halfway",
            Kind::RunEnded => "that run ends",
            Kind::FlowStarted => "a flow run starts",
            Kind::FlowStepStarted => "a flow run starts a step",
            Kind::FlowStepEnded => "a step's run ends: done, at its gate, or failed",
            Kind::FlowGate => "a flow run waits at a gate for you",
            Kind::FlowGateAnswered => "you approve a gate, or send the run back",
            Kind::FlowEnded => "a flow run ends",
            Kind::WorktreeCreated => "crystal makes a worktree",
            Kind::WorktreeRemoved => "crystal removes one",
            Kind::WorktreeHookFailed => "a worktree's create or delete hook fails",
            Kind::HandoffAdded => "a note goes in a worktree's handoff file",
            Kind::MemoryAdded => "an entry is added to a project's memory",
            Kind::MemoryForgotten => "an entry is forgotten",
            Kind::MemoryStale => "every file an entry is about has changed since it was said",
            Kind::MemoryPromoted => "an entry is written into the project's CLAUDE.md or AGENTS.md",
            Kind::MemoryDistilled => "the distiller has read what a session did",
            Kind::MemoryDistillFailed => "the distiller couldn't read what a session did",
            Kind::BacklogAdded => "an item goes on a project's backlog",
            Kind::BacklogClosed => "an item is marked done",
            Kind::PluginPaused => "a plugin is paused for failing",
            Kind::DaemonHandedOver => "the daemon is handed over to another crystal",
            Kind::DaemonRestarted => "the daemon, restarted cold, has started its sessions again",
        }
    }

    /// The event for a session's agent coming to do `activity`.
    pub fn of_activity(activity: Activity) -> Kind {
        match activity {
            Activity::Working => Kind::SessionWorking,
            Activity::Waiting => Kind::SessionWaiting,
            Activity::Done => Kind::SessionDone,
            Activity::Idle => Kind::SessionIdle,
        }
    }
}

impl From<Kind> for &'static str {
    fn from(kind: Kind) -> &'static str {
        kind.name()
    }
}

impl TryFrom<String> for Kind {
    type Error = String;

    fn try_from(name: String) -> Result<Kind, String> {
        Kind::named(&name).ok_or_else(|| format!("there's no event called {name}"))
    }
}

/// Something that happened. Only the fields its kind carries are there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Its place in the log: 1 for the first event, and one more for each
    /// after it. The daemon gives it as it writes the event down.
    #[serde(default)]
    pub seq: u64,
    /// When it happened, in milliseconds since the Unix epoch.
    #[serde(default)]
    pub at: u64,
    #[serde(rename = "event")]
    pub kind: Kind,
    /// The project it's about: its main worktree, or the directory itself
    /// outside git.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionAbout>,
    /// What it was before: a renamed session's old name, what its agent
    /// was doing before it changed, or the agent that let go of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunAbout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<FlowAbout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeAbout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<Entry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backlog: Option<BacklogItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginAbout>,
    /// A file kept with a task as it closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Artifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<HandoffAbout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<DaemonAbout>,
    /// The subagent a session's agent started, or that finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<Subagent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageAbout>,
    /// What the distiller made of what a session did, or why it couldn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distill: Option<DistillAbout>,
    /// The file it's about: the instructions file an entry of memory was
    /// written into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
}

/// The session an event is about, as it was then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionAbout {
    pub name: String,
    pub id: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    /// Its project's main worktree, when it runs in git.
    pub project: Option<PathBuf>,
    /// The worktree it runs in, when it runs in git.
    pub worktree: Option<PathBuf>,
    pub branch: Option<String>,
    pub activity: Option<Activity>,
    /// What it was asked to do, when it has a task.
    pub task: Option<String>,
    /// That task's number, when it has one: what a task's timeline takes
    /// its session's events by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<u64>,
    /// The word `ls` shows for it: `waiting`, `running`, `exited 0`.
    #[serde(default)]
    pub status: String,
    /// The agent that says what it's doing itself, while it holds the
    /// session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporter: Option<Reporter>,
}

/// A background task's run of `claude -p`: what it was asked as it starts,
/// the permissions it asks for and how they're answered, and how it went
/// once it has ended.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunAbout {
    /// The first line of the task's prompt, or of a follow-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The permission Claude asks for, or that was answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asking: Option<Asking>,
    /// The tool Claude used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolUse>,
    /// How the user answered it: `allow`, `deny` or `always`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<Answer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<bool>,
    /// The first line of Claude's answer, or of what went wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// What the task has cost so far, in US dollars, as Claude counts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// A flow run, and the step an event is about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowAbout {
    /// The run's name: `ship-1`.
    pub run: String,
    /// The flow it runs, by its name in the config file.
    pub flow: String,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// How the step, or the whole run, stands: `running`, `waiting`, `done`
    /// or `failed`; for a gate answered, `approved` or `sent back`.
    pub state: String,
    /// The first line of a step's answer or of why it failed, or the notes
    /// a gate was sent back with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
    /// What a step's run cost, or once the run ends, all of it, in US
    /// dollars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// A worktree crystal made or removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeAbout {
    pub path: PathBuf,
    pub branch: Option<String>,
    /// Its project's main worktree. A removed worktree's directory is gone,
    /// so git can't say which project it was in, unless crystal removed it.
    pub project: Option<PathBuf>,
    /// Why the hook run on it failed, for `worktree.hook_failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// A note added to a worktree's handoff file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffAbout {
    /// The handoff file.
    pub path: PathBuf,
    /// The note's first line.
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginAbout {
    pub name: String,
    pub why: String,
}

/// A message a session was sent with `crystal send`, or from the TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAbout {
    /// The session that sent it, by name, when another session did; `None`
    /// from the user, or a script.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// That session's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_id: Option<String>,
    /// The first line of what it says, after the line saying who sent it.
    pub line: String,
}

/// The daemon, handed over to another crystal: the version it runs now,
/// and how many sessions carried on through it. The version it ran before
/// is the event's `from`. Or restarted cold: how many sessions it started
/// again, and those that couldn't start, by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonAbout {
    pub version: String,
    pub sessions: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
}

/// A tool a background task's Claude used: its name, and the gist of what
/// it was given, like the command it ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUse {
    pub name: String,
    pub gist: String,
}

/// A pass of the distiller over what a session did: how many entries it
/// added to the project's memory, found there already and turned down,
/// and what it cost; or why it failed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DistillAbout {
    pub added: usize,
    pub again: usize,
    pub rejected: usize,
    pub cost_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

impl DistillAbout {
    /// What came of it, in a line: `2 added, 1 seen again ($0.0012)`, or
    /// why it failed.
    pub fn line(&self) -> String {
        if let Some(why) = &self.failed {
            return why.clone();
        }
        let mut line = format!("{} added", self.added);
        if self.again > 0 {
            line.push_str(&format!(", {} seen again", self.again));
        }
        if self.rejected > 0 {
            line.push_str(&format!(", {} rejected", self.rejected));
        }
        format!("{line}{}", cost(Some(self.cost_usd)))
    }
}

impl Event {
    /// An event of `kind` about nothing yet, for the constructors to fill
    /// in. The daemon gives it its `seq` and `at`.
    pub fn new(kind: Kind) -> Event {
        Event {
            seq: 0,
            at: 0,
            kind,
            project: None,
            session: None,
            from: None,
            task: None,
            run: None,
            flow: None,
            worktree: None,
            memory: None,
            backlog: None,
            plugin: None,
            artifact: None,
            handoff: None,
            daemon: None,
            subagent: None,
            message: None,
            distill: None,
            file: None,
        }
    }

    /// An event of `kind` about `session`, as it is now.
    pub fn about_session(kind: Kind, session: &SessionInfo) -> Event {
        let project = match &session.worktree {
            Some(worktree) => worktree.project_path.clone(),
            None => project::of(&session.cwd).path,
        };
        Event {
            project: Some(project),
            session: Some(SessionAbout::of(session)),
            ..Event::new(kind)
        }
    }

    /// An event of `kind` about the project whose main worktree is
    /// `project`.
    pub fn about_project(kind: Kind, project: PathBuf) -> Event {
        Event {
            project: Some(project),
            ..Event::new(kind)
        }
    }

    /// `session`'s agent went from doing `from` to doing `to`.
    pub fn activity(session: &SessionInfo, from: Option<Activity>, to: Activity) -> Event {
        let mut event = Event::about_session(Kind::of_activity(to), session);
        if let Some(about) = &mut event.session {
            about.activity = Some(to);
            about.status = to.to_string();
        }
        Event {
            from: from.map(|from| from.to_string()),
            ..event
        }
    }

    /// `session` was given another name; `from` is the one it had.
    pub fn renamed(session: &SessionInfo, from: &str) -> Event {
        Event {
            from: Some(from.to_string()),
            ..Event::about_session(Kind::SessionRenamed, session)
        }
    }

    /// The agent called `agent`, which said what `session` was doing
    /// itself, let go of it.
    pub fn released(session: &SessionInfo, agent: &str) -> Event {
        Event {
            from: Some(agent.to_string()),
            ..Event::about_session(Kind::SessionReleased, session)
        }
    }

    /// `session`'s program ended, as `status` says: `exited 0`, or
    /// `killed` for one stopped as it left the list.
    pub fn ended(session: &SessionInfo, status: String) -> Event {
        let mut event = Event::about_session(Kind::SessionEnded, session);
        if let Some(about) = &mut event.session {
            about.status = status;
        }
        event
    }

    /// `session` couldn't start again after a restart, for the reason its
    /// state gives, which its status says.
    pub fn start_failed(session: &SessionInfo) -> Event {
        let mut event = Event::about_session(Kind::SessionStartFailed, session);
        if let (Some(about), State::Failed { why }) = (&mut event.session, &session.state) {
            about.status = format!("couldn't start: {why}");
        }
        event
    }

    /// `session`'s task opened or closed, as `task` says.
    pub fn task(kind: Kind, session: &SessionInfo, task: TaskRecord) -> Event {
        Event {
            task: Some(task),
            ..Event::about_session(kind, session)
        }
    }

    /// A task no session works on: one made to start later, in `project`,
    /// or one cancelled before it started.
    pub fn pending_task(kind: Kind, project: PathBuf, task: TaskRecord) -> Event {
        Event {
            task: Some(task),
            ..Event::about_project(kind, project)
        }
    }

    /// `artifact` was kept with `session`'s task, `task`, as it closed.
    pub fn artifact(session: &SessionInfo, task: TaskRecord, artifact: Artifact) -> Event {
        Event {
            task: Some(task),
            artifact: Some(artifact),
            ..Event::about_session(Kind::TaskArtifact, session)
        }
    }

    /// `note` was added to the handoff file at `path`, of `session`'s
    /// worktree: by the session, or as its task closed.
    pub fn handoff(session: &SessionInfo, path: PathBuf, note: &str) -> Event {
        let handoff = HandoffAbout {
            path,
            note: first_line(note),
        };
        Event {
            handoff: Some(handoff),
            ..Event::about_session(Kind::HandoffAdded, session)
        }
    }

    /// The background task `session` started a run of Claude on `prompt`.
    pub fn run_started(session: &SessionInfo, prompt: &str) -> Event {
        let run = RunAbout {
            prompt: Some(first_line(prompt)),
            ..RunAbout::default()
        };
        Event {
            run: Some(run),
            ..Event::about_session(Kind::RunStarted, session)
        }
    }

    /// The run ended, with what it came to.
    pub fn run_ended(session: &SessionInfo, result: &TaskResult) -> Event {
        let run = RunAbout {
            failed: Some(result.failed),
            answer: Some(first_line(&result.text)),
            cost_usd: Some(result.cost_usd),
            ..RunAbout::default()
        };
        Event {
            run: Some(run),
            ..Event::about_session(Kind::RunEnded, session)
        }
    }

    /// The background task `session`'s Claude used `tool`.
    pub fn tool_use(session: &SessionInfo, tool: ToolUse) -> Event {
        let run = RunAbout {
            tool: Some(tool),
            ..RunAbout::default()
        };
        Event {
            run: Some(run),
            ..Event::about_session(Kind::RunToolUse, session)
        }
    }

    /// The background task `session` asks the user for a permission:
    /// `asking`.
    pub fn asking(session: &SessionInfo, asking: Asking) -> Event {
        let run = RunAbout {
            asking: Some(asking),
            ..RunAbout::default()
        };
        Event {
            run: Some(run),
            ..Event::about_session(Kind::RunAsking, session)
        }
    }

    /// The user answered the permission `session`'s background task asked
    /// for, `asking`, with `decision`.
    pub fn answered(session: &SessionInfo, asking: Option<Asking>, decision: Answer) -> Event {
        let run = RunAbout {
            asking,
            decision: Some(decision),
            ..RunAbout::default()
        };
        Event {
            run: Some(run),
            ..Event::about_session(Kind::RunAnswered, session)
        }
    }

    pub fn flow_started(run: &FlowRun) -> Event {
        Event::about_flow(Kind::FlowStarted, run, None, "running")
    }

    /// `run` started `step`, in `session`.
    pub fn step_started(run: &FlowRun, step: usize, session: Option<&SessionInfo>) -> Event {
        let event = Event::about_flow(Kind::FlowStepStarted, run, Some(step), "running");
        Event {
            session: session.map(SessionAbout::of),
            ..event
        }
    }

    /// The run of `step` ended: it's done, waits at its gate, or failed.
    /// `cost_usd` is what that run cost.
    pub fn step_ended(run: &FlowRun, step: usize, cost_usd: f64) -> Event {
        let state = run.steps[step].state;
        let mut event = Event::about_flow(Kind::FlowStepEnded, run, Some(step), state.word());
        if let Some(flow) = &mut event.flow {
            flow.said = run.steps[step].answer.as_deref().map(first_line);
            flow.cost_usd = Some(cost_usd);
        }
        event
    }

    /// `run` stopped at the gate after `step`.
    pub fn gate(run: &FlowRun, step: usize) -> Event {
        Event::about_flow(Kind::FlowGate, run, Some(step), "waiting")
    }

    /// The user answered the gate after `step`: went on, or, with `notes`,
    /// sent the run back.
    pub fn gate_answered(run: &FlowRun, step: usize, notes: Option<&str>) -> Event {
        let state = if notes.is_some() {
            "sent back"
        } else {
            "approved"
        };
        let mut event = Event::about_flow(Kind::FlowGateAnswered, run, Some(step), state);
        if let Some(flow) = &mut event.flow {
            flow.said = notes.map(first_line);
        }
        event
    }

    /// `run` ended: every step done, or one failed or was cancelled, which
    /// it names, with why it failed.
    pub fn flow_ended(run: &FlowRun) -> Event {
        let stopped = |state| run.steps.iter().position(|step| step.state == state);
        let failed = stopped(StepState::Failed);
        let step = stopped(StepState::Cancelled).or(failed);
        let mut event = Event::about_flow(Kind::FlowEnded, run, step, run.state().word());
        if let Some(flow) = &mut event.flow {
            flow.said = failed.and_then(|step| run.steps[step].answer.as_deref().map(first_line));
            flow.cost_usd = Some(run.cost_usd());
        }
        event
    }

    fn about_flow(kind: Kind, run: &FlowRun, step: Option<usize>, state: &str) -> Event {
        let flow = FlowAbout {
            run: run.name.clone(),
            flow: run.flow.name.clone(),
            goal: first_line(&run.goal),
            step: step.map(|step| run.step_name(step).to_string()),
            state: state.to_string(),
            said: None,
            cost_usd: None,
        };
        Event {
            flow: Some(flow),
            ..Event::about_project(kind, project::of(&run.cwd).path)
        }
    }

    /// A worktree was made at `path`, or removed from there.
    pub fn worktree(created: bool, path: &Path, branch: Option<&str>) -> Event {
        let kind = if created {
            Kind::WorktreeCreated
        } else {
            Kind::WorktreeRemoved
        };
        let project = created.then(|| project::of(path).path);
        let worktree = WorktreeAbout {
            path: path.to_path_buf(),
            branch: branch.map(String::from),
            project: project.clone(),
            why: None,
        };
        Event {
            project,
            worktree: Some(worktree),
            ..Event::new(kind)
        }
    }

    /// The daemon removed the worktree at `path`, of the project whose main
    /// worktree is `project`.
    pub fn worktree_removed(path: &Path, branch: Option<&str>, project: &Path) -> Event {
        let mut event = Event::worktree(false, path, branch);
        event.project = Some(project.to_path_buf());
        if let Some(worktree) = &mut event.worktree {
            worktree.project = Some(project.to_path_buf());
        }
        event
    }

    /// The hook run on `worktree`, made or removed, failed, for `why`: see
    /// [`crate::worktree_hooks`].
    pub fn worktree_hook_failed(worktree: &WorktreeAbout, why: &str) -> Event {
        Event {
            project: worktree.project.clone(),
            worktree: Some(WorktreeAbout {
                why: Some(why.to_string()),
                ..worktree.clone()
            }),
            ..Event::new(Kind::WorktreeHookFailed)
        }
    }

    /// `entry` was added to `project`'s memory, or forgotten.
    pub fn memory(kind: Kind, project: PathBuf, entry: Entry) -> Event {
        Event {
            memory: Some(entry),
            ..Event::about_project(kind, project)
        }
    }

    /// `entry` of `project`'s memory was written into `file`, the
    /// project's CLAUDE.md or AGENTS.md.
    pub fn promoted(project: PathBuf, entry: Entry, file: PathBuf) -> Event {
        Event {
            file: Some(file),
            ..Event::memory(Kind::MemoryPromoted, project, entry)
        }
    }

    /// The distiller read what `session` did: `distill` says what came of
    /// it, and with its `failed`, why it couldn't.
    pub fn distilled(session: &SessionInfo, distill: DistillAbout) -> Event {
        let kind = if distill.failed.is_some() {
            Kind::MemoryDistillFailed
        } else {
            Kind::MemoryDistilled
        };
        Event {
            distill: Some(distill),
            ..Event::about_session(kind, session)
        }
    }

    /// `item` went on `project`'s backlog, or was marked done.
    pub fn backlog(kind: Kind, project: PathBuf, item: BacklogItem) -> Event {
        Event {
            backlog: Some(item),
            ..Event::about_project(kind, project)
        }
    }

    pub fn plugin_paused(name: &str, why: &str) -> Event {
        let plugin = PluginAbout {
            name: name.to_string(),
            why: why.to_string(),
        };
        Event {
            plugin: Some(plugin),
            ..Event::new(Kind::PluginPaused)
        }
    }

    /// `session`'s agent started `subagent`, or it finished, as `kind`
    /// says; `session` counts it already.
    pub fn subagent(kind: Kind, session: &SessionInfo, subagent: Subagent) -> Event {
        Event {
            subagent: Some(subagent),
            ..Event::about_session(kind, session)
        }
    }

    /// `session` was sent `text`: by the session `from`, whose header
    /// starts it, or by the user.
    pub fn message(session: &SessionInfo, from: Option<&Sender>, text: &str) -> Event {
        let said = match from {
            Some(_) => text.split_once('\n').map_or("", |(_, said)| said),
            None => text,
        };
        let message = MessageAbout {
            from: from.map(|sender| sender.name.clone()),
            from_id: from.map(|sender| sender.id.clone()),
            line: first_line(said),
        };
        Event {
            message: Some(message),
            ..Event::about_session(Kind::SessionMessage, session)
        }
    }

    /// The daemon, which ran crystal `from`, was handed over to this one,
    /// and `sessions` carried on through it.
    pub fn handed_over(from: &str, version: &str, sessions: usize) -> Event {
        Event {
            from: Some(from.to_string()),
            daemon: Some(DaemonAbout {
                version: version.to_string(),
                sessions,
                failed: Vec::new(),
            }),
            ..Event::new(Kind::DaemonHandedOver)
        }
    }

    /// The daemon, of crystal `version`, restarted cold and started the
    /// sessions written down again: `back` of them did, and those called
    /// `failed` couldn't.
    pub fn restarted(version: &str, back: usize, failed: Vec<String>) -> Event {
        Event {
            daemon: Some(DaemonAbout {
                version: version.to_string(),
                sessions: back,
                failed,
            }),
            ..Event::new(Kind::DaemonRestarted)
        }
    }

    /// What it's about, in a word: the session's name, the flow run's, the
    /// plugin's, the daemon, or else the project's.
    pub fn subject(&self) -> String {
        if let Some(session) = &self.session {
            return session.name.clone();
        }
        if let Some(flow) = &self.flow {
            return flow.run.clone();
        }
        if let Some(plugin) = &self.plugin {
            return plugin.name.clone();
        }
        if self.daemon.is_some() {
            return "daemon".to_string();
        }
        match &self.project {
            Some(project) => project::name_of(project),
            None => "-".to_string(),
        }
    }

    /// What happened, in a line of its own: its subject and what it says,
    /// or its name when it says nothing more: `claude-2: working → waiting`,
    /// `claude-2: session.archived`. A plugin's hooks find it in
    /// `CRYSTAL_EVENT_TEXT`.
    pub fn line(&self) -> String {
        let text = self.text();
        let text = if text.is_empty() {
            self.kind.name()
        } else {
            &text
        };
        format!("{}: {text}", self.subject())
    }

    /// What happened, in words, to follow its subject.
    pub fn text(&self) -> String {
        let said = |text: Option<&str>| text.map(|text| format!(": {text}")).unwrap_or_default();
        match self.kind {
            Kind::SessionStarted | Kind::SessionOpenedInTerminal => {
                self.session.as_ref().map_or(String::new(), |session| {
                    let command: Vec<String> =
                        session.command.iter().map(|a| shell::quote(a)).collect();
                    command.join(" ")
                })
            }
            Kind::SessionUnarchived => "back from the archive".to_string(),
            Kind::SessionRenamed => format!("was {}", self.from.as_deref().unwrap_or("?")),
            Kind::SessionWorking | Kind::SessionWaiting | Kind::SessionDone | Kind::SessionIdle => {
                let now = self.session.as_ref().map_or("", |session| &session.status);
                let changed = match &self.from {
                    Some(from) => format!("{from} → {now}"),
                    None => now.to_string(),
                };
                // What an agent that reports for itself says it waits for.
                let message = self
                    .session
                    .as_ref()
                    .and_then(|session| session.reporter.as_ref()?.message.as_deref())
                    .filter(|_| self.kind == Kind::SessionWaiting);
                format!("{changed}{}", said(message))
            }
            Kind::SessionClaimed => self
                .session
                .as_ref()
                .and_then(|session| session.reporter.as_ref())
                .map_or(String::new(), |reporter| format!("by {}", reporter.agent)),
            Kind::SessionReleased => format!("by {}", self.from.as_deref().unwrap_or("?")),
            Kind::SubagentStarted | Kind::SubagentStopped => {
                self.subagent.as_ref().map_or(String::new(), |subagent| {
                    match &subagent.agent_type {
                        Some(agent_type) => format!("{agent_type} ({})", subagent.id),
                        None => subagent.id.clone(),
                    }
                })
            }
            Kind::SessionMessage => self.message.as_ref().map_or(String::new(), |message| {
                let from = message.from.as_deref().unwrap_or("you");
                format!("from {from}: {}", message.line)
            }),
            Kind::SessionBell => "rang the bell".to_string(),
            Kind::SessionCopyDropped => {
                "copied while nobody watched: not put on the clipboard".to_string()
            }
            Kind::SessionEnded | Kind::SessionStartFailed => self
                .session
                .as_ref()
                .map_or(String::new(), |session| session.status.clone()),
            Kind::TaskArtifact => self.artifact.as_ref().map_or(String::new(), |artifact| {
                format!(
                    "kept {} ({})",
                    artifact.name,
                    artifacts::size(artifact.bytes)
                )
            }),
            Kind::HandoffAdded => self
                .handoff
                .as_ref()
                .map_or(String::new(), |handoff| handoff.note.clone()),
            Kind::TaskOpened
            | Kind::TaskStarted
            | Kind::TaskWaiting
            | Kind::TaskReminded
            | Kind::TaskClosed => self.task.as_ref().map_or(String::new(), |task| {
                let goal = first_line(&task.goal);
                match &task.outcome {
                    None if task.pending => format!("{goal}, to start later"),
                    None => goal,
                    Some(outcome) => {
                        let summary = (!outcome.summary.is_empty()).then_some(&*outcome.summary);
                        format!("{}{}", outcome.state().word(), said(summary))
                    }
                }
            }),
            Kind::RunStarted
            | Kind::RunToolUse
            | Kind::RunAsking
            | Kind::RunAnswered
            | Kind::RunInterrupted
            | Kind::RunEnded => self.run.as_ref().map_or(String::new(), |run| {
                if let Some(prompt) = &run.prompt {
                    return prompt.clone();
                }
                if let Some(tool) = &run.tool {
                    return format!("{} {}", tool.name, tool.gist);
                }
                let asked = run
                    .asking
                    .as_ref()
                    .map(|asking| format!("{} {}", asking.tool, asking.gist));
                match (run.decision, asked) {
                    (Some(decision), asked) => {
                        format!("{}{}", decision_word(decision), said(asked.as_deref()))
                    }
                    (None, Some(asked)) => asked,
                    (None, None) if self.kind == Kind::RunEnded => {
                        let how = if run.failed == Some(true) {
                            "failed"
                        } else {
                            "done"
                        };
                        format!("{how}{}{}", cost(run.cost_usd), said(run.answer.as_deref()))
                    }
                    (None, None) => String::new(),
                }
            }),
            Kind::FlowStarted
            | Kind::FlowStepStarted
            | Kind::FlowStepEnded
            | Kind::FlowGate
            | Kind::FlowGateAnswered
            | Kind::FlowEnded => self.flow.as_ref().map_or(String::new(), |flow| {
                if self.kind == Kind::FlowStarted {
                    return format!("{}: {}", flow.flow, flow.goal);
                }
                let step = flow
                    .step
                    .as_deref()
                    .map(|step| format!("{step} "))
                    .unwrap_or_default();
                format!(
                    "{step}{}{}{}",
                    flow.state,
                    cost(flow.cost_usd),
                    said(flow.said.as_deref())
                )
            }),
            Kind::WorktreeCreated | Kind::WorktreeRemoved => {
                self.worktree.as_ref().map_or(String::new(), |worktree| {
                    let path = shell::home_relative(&worktree.path);
                    match &worktree.branch {
                        Some(branch) => format!("{path} on {branch}"),
                        None => path,
                    }
                })
            }
            Kind::WorktreeHookFailed => self.worktree.as_ref().map_or(String::new(), |worktree| {
                let path = shell::home_relative(&worktree.path);
                let why = worktree.why.as_deref().unwrap_or("it failed");
                format!("{path}: {why}")
            }),
            Kind::MemoryAdded
            | Kind::MemoryForgotten
            | Kind::MemoryStale
            | Kind::MemoryPromoted => self.memory.as_ref().map_or(String::new(), |entry| {
                let into = self
                    .file
                    .as_ref()
                    .map(|file| format!(" → {}", shell::home_relative(file)))
                    .unwrap_or_default();
                format!(
                    "{} ({}) {}{into}",
                    entry.id,
                    entry.kind,
                    memory::title(&entry.text)
                )
            }),
            Kind::MemoryDistilled | Kind::MemoryDistillFailed => self
                .distill
                .as_ref()
                .map_or(String::new(), DistillAbout::line),
            Kind::BacklogAdded | Kind::BacklogClosed => {
                self.backlog.as_ref().map_or(String::new(), |item| {
                    format!("#{} {}", item.number, first_line(&item.text))
                })
            }
            Kind::SessionRemoved | Kind::SessionArchived => String::new(),
            Kind::PluginPaused => self
                .plugin
                .as_ref()
                .map_or(String::new(), |plugin| plugin.why.clone()),
            Kind::DaemonHandedOver => self.daemon.as_ref().map_or(String::new(), |daemon| {
                let from = self.from.as_deref().unwrap_or("?");
                let sessions = match daemon.sessions {
                    1 => "1 session".to_string(),
                    count => format!("{count} sessions"),
                };
                format!(
                    "from crystal {from} to {}, {sessions} carried on",
                    daemon.version
                )
            }),
            Kind::DaemonRestarted => self.daemon.as_ref().map_or(String::new(), |daemon| {
                let back = match daemon.sessions {
                    1 => "1 session".to_string(),
                    count => format!("{count} sessions"),
                };
                let failed = match daemon.failed.as_slice() {
                    [] => String::new(),
                    names => format!(", {} couldn't start: {}", names.len(), names.join(", ")),
                };
                format!(
                    "crystal {} started cold: {back} back{failed}",
                    daemon.version
                )
            }),
        }
    }
}

impl SessionAbout {
    pub fn of(session: &SessionInfo) -> SessionAbout {
        let worktree = session.worktree.as_ref();
        SessionAbout {
            name: session.name.clone(),
            id: session.id.clone(),
            command: session.command.clone(),
            cwd: session.cwd.clone(),
            project: worktree.map(|worktree| worktree.project_path.clone()),
            worktree: worktree.map(|worktree| worktree.path.clone()),
            branch: worktree.and_then(|worktree| worktree.branch.clone()),
            activity: session.activity,
            task: session.task.as_ref().map(|task| task.goal.clone()),
            task_id: session.task.as_ref().and_then(|task| task.id),
            status: session.status(),
            reporter: session.reporter.clone(),
        }
    }
}

/// A made-up event of `kind`, with all that kind carries filled in, about
/// `session`, or without one, a made-up session in `dir`: what `crystal
/// plugin run --event` tries a plugin's hooks on.
pub fn example(kind: Kind, session: Option<&SessionInfo>, dir: &Path) -> Event {
    let session = match session {
        Some(session) => session.clone(),
        None => SessionInfo {
            stopped_idle: false,
            name: "example".into(),
            id: "example".into(),
            command: vec!["claude".into()],
            cwd: dir.to_path_buf(),
            pid: None,
            state: State::Running,
            activity: Some(Activity::Idle),
            worktree: None,
            changed: 0,
            front: None,
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
        },
    };
    let now = now_ms() / 1000;
    let goal = "Fix the login redirect";
    let task = TaskRecord {
        goal: goal.into(),
        session: session.name.clone(),
        project: project::of(dir).name,
        branch: session
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.branch.clone()),
        background: false,
        backlog: None,
        pending: false,
        waiting: kind == Kind::TaskWaiting,
        created: now,
        outcome: None,
        id: Some(12),
        artifacts: Vec::new(),
        brief: Default::default(),
    };
    let asking = Asking {
        tool: "Bash".into(),
        gist: "cargo test".into(),
    };
    let at_step = |step: Option<&str>, state: &str| FlowAbout {
        run: "ship-1".into(),
        flow: "ship".into(),
        goal: goal.into(),
        step: step.map(String::from),
        state: state.into(),
        said: None,
        cost_usd: None,
    };
    let flow = |flow: FlowAbout| Event {
        flow: Some(flow),
        ..Event::about_project(kind, project::of(dir).path)
    };
    let entry = Entry {
        id: 1,
        kind: memory::Kind::Gotcha,
        text: "The ledger tests need the database up: make db".into(),
        files: vec!["Makefile".into()],
        source: memory::Source::User,
        created: now,
        seen: 1,
        last_seen: now,
        anchors: Default::default(),
        checkout: None,
    };
    let item = BacklogItem {
        number: 1,
        text: "Retry the webhook on a timeout".into(),
        body: String::new(),
        tags: Vec::new(),
        done: kind == Kind::BacklogClosed,
        created: now,
        closed: (kind == Kind::BacklogClosed).then_some(now),
    };
    let event = match kind {
        Kind::SessionStarted
        | Kind::SessionRemoved
        | Kind::SessionArchived
        | Kind::SessionUnarchived
        | Kind::SessionOpenedInTerminal
        | Kind::SessionBell
        | Kind::SessionCopyDropped => Event::about_session(kind, &session),
        Kind::SessionRenamed => Event::renamed(&session, "old-name"),
        Kind::SessionWorking => Event::activity(&session, Some(Activity::Idle), Activity::Working),
        Kind::SessionWaiting => {
            Event::activity(&session, Some(Activity::Working), Activity::Waiting)
        }
        Kind::SessionDone => Event::activity(&session, Some(Activity::Working), Activity::Done),
        Kind::SessionIdle => Event::activity(&session, Some(Activity::Done), Activity::Idle),
        Kind::SessionEnded => Event::ended(&session, "exited 0".into()),
        Kind::SessionStartFailed => {
            let why = format!("its directory, {}, isn't there", dir.display());
            Event::start_failed(&SessionInfo {
                state: State::Failed { why },
                ..session.clone()
            })
        }
        Kind::SessionClaimed => {
            let reporter = Reporter {
                agent: "my-agent".into(),
                message: None,
                resume: Some(vec!["my-agent".into(), "--resume".into(), "s1".into()]),
                source: None,
            };
            let session = SessionInfo {
                reporter: Some(reporter),
                ..session
            };
            Event::about_session(kind, &session)
        }
        Kind::SessionReleased => Event::released(&session, "my-agent"),
        Kind::SubagentStarted | Kind::SubagentStopped => {
            let subagent = Subagent {
                id: "a1b2c3".into(),
                agent_type: Some("Explore".into()),
            };
            let subagents = u32::from(kind == Kind::SubagentStarted);
            Event::subagent(
                kind,
                &SessionInfo {
                    subagents,
                    ..session
                },
                subagent,
            )
        }
        Kind::SessionMessage => {
            let from = Sender::new("p8w2…", "scout", None);
            let text = messages::compose(&from, "The codec moved to crates/codec");
            Event::message(&session, Some(&from), &text)
        }
        Kind::TaskOpened | Kind::TaskStarted | Kind::TaskWaiting | Kind::TaskReminded => {
            Event::task(kind, &session, task)
        }
        Kind::TaskClosed => {
            let outcome = TaskOutcome::new(TaskState::Done, "Fixed it, with a test", now);
            let task = TaskRecord {
                outcome: Some(outcome),
                ..task
            };
            Event::task(kind, &session, task)
        }
        Kind::TaskArtifact => {
            let kept = Artifact {
                kind: ArtifactKind::File,
                name: "plan.md".into(),
                path: dir.join(".crystal-state/tasks/t12/plan.md"),
                bytes: 2048,
            };
            Event::artifact(&session, task, kept)
        }
        Kind::HandoffAdded => {
            let path = dir.join(".crystal/handoff.md");
            Event::handoff(
                &session,
                path,
                "The ledger tests need the database up: make db",
            )
        }
        Kind::RunStarted => Event::run_started(&session, goal),
        Kind::RunToolUse => {
            let tool = ToolUse {
                name: "Bash".into(),
                gist: "cargo test".into(),
            };
            Event::tool_use(&session, tool)
        }
        Kind::RunAsking => Event::asking(&session, asking),
        Kind::RunAnswered => Event::answered(&session, Some(asking), Answer::Allow),
        Kind::RunInterrupted => Event::about_session(kind, &session),
        Kind::RunEnded => {
            let result = TaskResult {
                text: "Fixed it, with a test".into(),
                failed: false,
                conversation: None,
                cost_usd: 0.0421,
                runs: 1,
            };
            Event::run_ended(&session, &result)
        }
        Kind::FlowStarted => flow(at_step(None, "running")),
        Kind::FlowStepStarted => flow(at_step(Some("build"), "running")),
        Kind::FlowStepEnded => flow(FlowAbout {
            said: Some("Built it".into()),
            cost_usd: Some(0.0421),
            ..at_step(Some("build"), "done")
        }),
        Kind::FlowGate => flow(at_step(Some("review"), "waiting")),
        Kind::FlowGateAnswered => flow(at_step(Some("review"), "approved")),
        Kind::FlowEnded => flow(at_step(None, "done")),
        Kind::WorktreeCreated | Kind::WorktreeRemoved => {
            let path = session
                .worktree
                .as_ref()
                .map_or(dir, |worktree| &worktree.path);
            Event::worktree(kind == Kind::WorktreeCreated, path, Some("fix-login"))
        }
        Kind::WorktreeHookFailed => {
            let path = session
                .worktree
                .as_ref()
                .map_or(dir, |worktree| &worktree.path);
            let made = Event::worktree(true, path, Some("fix-login"));
            let worktree = made.worktree.expect("a worktree event has its worktree");
            let why = "the worktree create hook exited with exit status: 1";
            Event::worktree_hook_failed(&worktree, why)
        }
        Kind::MemoryAdded | Kind::MemoryForgotten | Kind::MemoryStale => {
            Event::memory(kind, project::of(dir).path, entry)
        }
        Kind::MemoryPromoted => {
            let project = project::of(dir).path;
            Event::promoted(project.clone(), entry, project.join("CLAUDE.md"))
        }
        Kind::MemoryDistilled | Kind::MemoryDistillFailed => {
            let failed = (kind == Kind::MemoryDistillFailed)
                .then(|| "claude -p ended with exit status: 1".to_string());
            let distill = DistillAbout {
                added: 2,
                again: 1,
                rejected: 0,
                cost_usd: 0.0012,
                failed,
            };
            Event::distilled(&session, distill)
        }
        Kind::BacklogAdded | Kind::BacklogClosed => {
            Event::backlog(kind, project::of(dir).path, item)
        }
        Kind::PluginPaused => Event::plugin_paused("example", "it failed 5 times in a row"),
        Kind::DaemonHandedOver => Event::handed_over("0.3.0", "0.4.0", 3),
        Kind::DaemonRestarted => Event::restarted("0.4.0", 5, vec!["docs".into()]),
    };
    Event {
        at: now_ms(),
        ..event
    }
}

/// Which events a reader wants: every one, but for what it says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    /// Names or patterns, the way a plugin's `on` takes them:
    /// `session.waiting`, `task.*`, `*`. None takes every kind.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Only those about the session with this name or id.
    #[serde(default)]
    pub session: Option<String>,
    /// Only those about the project whose main worktree this is.
    #[serde(default)]
    pub project: Option<PathBuf>,
    /// Only those about the task with this number: the task itself, and
    /// its session while it works on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<u64>,
}

impl Filter {
    pub fn matches(&self, event: &Event) -> bool {
        let kind = event.kind.name();
        let kind_wanted = self.kinds.is_empty()
            || self
                .kinds
                .iter()
                .any(|pattern| plugin_manifest::matches(pattern, kind));
        let session_wanted = self.session.as_ref().is_none_or(|wanted| {
            event
                .session
                .as_ref()
                .is_some_and(|session| session.name == *wanted || session.id == *wanted)
        });
        let project_wanted = self.project.is_none() || self.project == event.project;
        let task_wanted = self
            .task
            .is_none_or(|wanted| Scope::Task(wanted).matches(event));
        kind_wanted && session_wanted && project_wanted && task_wanted
    }
}

/// What a timeline is about: everything, or one session, task or project.
/// The log is read a page at a time for one (see [`crate::db::Db`]'s
/// `events_before`), and the events that happen meanwhile are taken by
/// [`Scope::matches`]: the two say the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    /// The session with this id, whatever it's called: what's about it,
    /// and the messages it sent.
    Session(String),
    /// The task with this number: its own events, and its session's while
    /// it has the task.
    Task(u64),
    /// The project whose main worktree this is.
    Project(PathBuf),
}

impl Scope {
    pub fn matches(&self, event: &Event) -> bool {
        match self {
            Scope::All => true,
            Scope::Session(id) => {
                let about = event.session.as_ref().is_some_and(|s| s.id == *id);
                let sent = event.message.as_ref();
                about || sent.is_some_and(|message| message.from_id.as_ref() == Some(id))
            }
            Scope::Task(id) => {
                let task = event.task.as_ref().and_then(|task| task.id);
                let session = event.session.as_ref().and_then(|s| s.task_id);
                task == Some(*id) || session == Some(*id)
            }
            Scope::Project(path) => event.project.as_ref() == Some(path),
        }
    }
}

/// Where in the log a reader starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Since {
    /// After the event with this `seq`: 0 for the whole log.
    Seq(u64),
    /// At this time or after, in milliseconds since the Unix epoch.
    At(u64),
}

/// Refuses a pattern that matches none of the events, which would wait
/// for nothing, without a word.
pub fn check_pattern(pattern: &str) -> anyhow::Result<()> {
    let known = Kind::ALL
        .iter()
        .any(|kind| plugin_manifest::matches(pattern, kind.name()));
    if !known {
        let names: Vec<&str> = Kind::ALL.iter().map(|kind| kind.name()).collect();
        anyhow::bail!(
            "`{pattern}` matches no event; they are {}",
            names.join(", ")
        );
    }
    Ok(())
}

/// Now, in milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// The first line of `text` with something on it, cut short past a few
/// hundred characters: enough to say what it was, kept to one log line.
fn first_line(text: &str) -> String {
    const LONGEST: usize = 200;
    let line = text.lines().map(str::trim).find(|line| !line.is_empty());
    let line = line.unwrap_or("");
    match line.char_indices().nth(LONGEST) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

/// How a permission was answered, in words.
fn decision_word(decision: Answer) -> &'static str {
    match decision {
        Answer::Allow => "allowed",
        Answer::Deny => "denied",
        Answer::Always => "allowed always",
    }
}

/// A cost, as ` ($0.0123)`, when there is one.
fn cost(cost_usd: Option<f64>) -> String {
    cost_usd.map_or(String::new(), |cost| format!(" (${cost:.4})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Worktree;

    fn session() -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            name: "claude".into(),
            id: "s1".into(),
            command: vec!["claude".into()],
            cwd: "/code/app".into(),
            pid: None,
            state: State::Running,
            activity: Some(Activity::Working),
            worktree: Some(Worktree {
                project: "app".into(),
                project_path: "/code/app".into(),
                path: "/code/app".into(),
                main: true,
                branch: Some("main".into()),
                in_progress: None,
            }),
            changed: 0,
            front: None,
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

    #[test]
    fn every_kind_has_a_name_of_its_own_that_reads_back() {
        let mut names: Vec<&str> = Kind::ALL.iter().map(|kind| kind.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Kind::ALL.len());
        for kind in Kind::ALL {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.name()));
            assert_eq!(serde_json::from_str::<Kind>(&json).unwrap(), kind);
        }
        assert!(serde_json::from_str::<Kind>("\"session.teleported\"").is_err());
    }

    #[test]
    fn a_session_event_keeps_the_fields_plugins_have_always_read() {
        let event = Event::activity(&session(), Some(Activity::Working), Activity::Waiting);
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event"], "session.waiting");
        assert_eq!(json["project"], "/code/app");
        assert_eq!(json["from"], "working");
        let about = &json["session"];
        assert_eq!(about["name"], "claude");
        assert_eq!(about["id"], "s1");
        assert_eq!(about["branch"], "main");
        assert_eq!(about["worktree"], "/code/app");
        assert_eq!(about["activity"], "waiting");
        assert_eq!(about["status"], "waiting");
        // What a kind doesn't carry isn't there at all.
        assert!(json.get("task").is_none() && json.get("flow").is_none());
    }

    #[test]
    fn a_worktree_event_keeps_its_worktree_as_plugins_have_read_it() {
        let removed = Event::worktree(false, Path::new("/code/app.worktrees/x"), Some("x"));
        let json = serde_json::to_value(&removed).unwrap();
        assert_eq!(json["event"], "worktree.removed");
        assert_eq!(json["worktree"]["path"], "/code/app.worktrees/x");
        assert_eq!(json["worktree"]["branch"], "x");
        assert!(json["worktree"]["project"].is_null());
    }

    #[test]
    fn a_filter_takes_kinds_by_pattern_and_a_session_by_name_or_id() {
        let event = Event::about_session(Kind::SessionStarted, &session());
        let filter = |kinds: &[&str], session: Option<&str>, project: Option<&str>| Filter {
            kinds: kinds.iter().map(|kind| kind.to_string()).collect(),
            session: session.map(String::from),
            project: project.map(PathBuf::from),
            task: None,
        };
        assert!(filter(&[], None, None).matches(&event));
        assert!(filter(&["session.*"], None, None).matches(&event));
        assert!(filter(&["task.*", "session.started"], None, None).matches(&event));
        assert!(!filter(&["session.waiting"], None, None).matches(&event));
        assert!(filter(&[], Some("claude"), None).matches(&event));
        assert!(filter(&[], Some("s1"), None).matches(&event));
        assert!(!filter(&[], Some("codex"), None).matches(&event));
        assert!(filter(&[], None, Some("/code/app")).matches(&event));
        assert!(!filter(&[], None, Some("/code/other")).matches(&event));
        let memory = Event::new(Kind::MemoryAdded);
        assert!(!filter(&[], Some("claude"), None).matches(&memory));
    }

    #[test]
    fn a_filter_takes_a_task_and_its_session_while_it_works_on_it() {
        use crate::protocol::TaskInfo;
        let by_task = Filter {
            task: Some(12),
            ..Filter::default()
        };
        let mut working = session();
        working.task = Some(TaskInfo {
            id: Some(12),
            goal: "Port the codec".into(),
            background: false,
            backlog: None,
            waiting: false,
            created: 0,
            outcome: None,
            brief: Default::default(),
        });
        let about = Event::about_session(Kind::SessionWaiting, &working);
        assert_eq!(about.session.as_ref().unwrap().task_id, Some(12));
        assert!(by_task.matches(&about));
        // Another task's session, and a session with none, are left out.
        working.task.as_mut().unwrap().id = Some(13);
        assert!(!by_task.matches(&Event::about_session(Kind::SessionWaiting, &working)));
        assert!(!by_task.matches(&Event::about_session(Kind::SessionWaiting, &session())));
        // The task's own events carry it.
        let closed = Event {
            task: Some(TaskRecord {
                id: Some(12),
                goal: "Port the codec".into(),
                session: "porter".into(),
                project: "app".into(),
                branch: None,
                background: false,
                backlog: None,
                pending: false,
                waiting: false,
                created: 0,
                outcome: None,
                artifacts: Vec::new(),
                brief: Default::default(),
            }),
            ..Event::new(Kind::TaskClosed)
        };
        assert!(by_task.matches(&closed));
    }

    #[test]
    fn a_pattern_that_matches_nothing_is_refused() {
        assert!(check_pattern("flow.*").is_ok());
        assert!(check_pattern("*").is_ok());
        let err = check_pattern("sesion.*").unwrap_err();
        assert!(err.to_string().contains("matches no event"), "{err}");
    }

    #[test]
    fn an_event_says_what_happened_in_a_line() {
        let info = session();
        let waiting = Event::activity(&info, Some(Activity::Working), Activity::Waiting);
        assert_eq!(
            (waiting.subject(), waiting.text()),
            ("claude".into(), "working → waiting".into())
        );
        let ended = Event::ended(
            &SessionInfo {
                state: State::Exited { code: 3 },
                ..session()
            },
            "exited 3".into(),
        );
        assert_eq!(ended.text(), "exited 3");
        let result = TaskResult {
            text: "It's fixed\nand tested".into(),
            failed: false,
            conversation: None,
            cost_usd: 0.25,
            runs: 1,
        };
        assert_eq!(
            Event::run_ended(&info, &result).text(),
            "done ($0.2500): It's fixed"
        );
        assert_eq!(Event::renamed(&info, "old").text(), "was old");
        let paused = Event::plugin_paused("notes", "it kept failing");
        assert_eq!(
            (paused.subject(), paused.text()),
            ("notes".into(), "it kept failing".into())
        );
    }

    #[test]
    fn an_agent_that_reports_for_itself_is_named_and_says_what_it_waits_for() {
        let info = SessionInfo {
            reporter: Some(Reporter {
                agent: "pi".into(),
                message: Some("approve the deploy".into()),
                resume: None,
                source: None,
            }),
            activity: Some(Activity::Waiting),
            ..session()
        };
        let claimed = Event::about_session(Kind::SessionClaimed, &info);
        assert_eq!(claimed.text(), "by pi");
        let json = serde_json::to_value(&claimed).unwrap();
        assert_eq!(json["session"]["reporter"]["agent"], "pi");
        let waiting = Event::activity(&info, Some(Activity::Working), Activity::Waiting);
        assert_eq!(waiting.text(), "working → waiting: approve the deploy");
        let released = Event::released(&session(), "pi");
        assert_eq!(released.text(), "by pi");
        let json = serde_json::to_value(&released).unwrap();
        assert!(json["session"].get("reporter").is_none());
    }

    #[test]
    fn the_events_added_later_carry_what_they_say_where_plugins_read_it() {
        let dir = Path::new("/code/app");
        let json = |kind| serde_json::to_value(example(kind, Some(&session()), dir)).unwrap();
        let tool = json(Kind::RunToolUse);
        assert_eq!(tool["run"]["tool"]["name"], "Bash");
        assert_eq!(tool["run"]["tool"]["gist"], "cargo test");
        let distilled = json(Kind::MemoryDistilled);
        assert_eq!(distilled["session"]["name"], "claude");
        assert_eq!(distilled["distill"]["added"], 2);
        assert!(distilled["distill"].get("failed").is_none());
        let failed = json(Kind::MemoryDistillFailed);
        assert_eq!(
            failed["distill"]["failed"],
            "claude -p ended with exit status: 1"
        );
        assert_eq!(json(Kind::MemoryPromoted)["file"], "/code/app/CLAUDE.md");
        assert_eq!(
            json(Kind::TaskReminded)["task"]["goal"],
            "Fix the login redirect"
        );
        let promoted = example(Kind::MemoryPromoted, None, dir);
        assert!(
            promoted.text().ends_with(" → /code/app/CLAUDE.md"),
            "{}",
            promoted.text()
        );
        let distilled = example(Kind::MemoryDistilled, Some(&session()), dir);
        assert_eq!(distilled.line(), "claude: 2 added, 1 seen again ($0.0012)");
        let archived = Event::about_session(Kind::SessionArchived, &session());
        assert_eq!(archived.line(), "claude: session.archived");
        for kind in Kind::ALL {
            assert!(!kind.about().is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn a_long_answer_is_kept_to_the_start_of_its_first_line() {
        assert_eq!(first_line("\n  first  \nsecond"), "first");
        let long = "x".repeat(300);
        assert_eq!(first_line(&long).chars().count(), 201);
    }

    #[test]
    fn every_kind_has_an_example_with_what_it_carries() {
        let dir = Path::new("/code/app");
        for kind in Kind::ALL {
            let event = example(kind, Some(&session()), dir);
            assert_eq!(event.kind, kind);
            // The name says all there is to say of these.
            let said_by_name = [
                Kind::SessionRemoved,
                Kind::SessionArchived,
                Kind::RunInterrupted,
            ]
            .contains(&kind);
            assert!(!event.text().is_empty() || said_by_name, "{kind:?}");
        }
        let closed = example(Kind::TaskClosed, None, dir);
        assert_eq!(closed.session.unwrap().name, "example");
        assert!(closed.task.unwrap().outcome.is_some());
    }

    #[test]
    fn the_readme_lists_every_event() {
        let readme = include_str!("../README.md");
        let table = readme
            .split("| Event | When |")
            .nth(1)
            .expect("the README has a table of events");
        let listed: Vec<&str> = table
            .lines()
            .skip(2)
            .take_while(|line| line.starts_with('|'))
            .filter_map(|line| line.split('`').nth(1))
            .collect();
        let names: Vec<&str> = Kind::ALL.iter().map(|kind| kind.name()).collect();
        assert_eq!(listed, names);
    }
}
