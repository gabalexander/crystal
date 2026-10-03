//! A program running in a PTY of its own, or a task: Claude Code run
//! without a terminal.

use crate::agent_screen::{self, Looks, ScreenWatch};
use crate::agents;
use crate::codex::Rollouts;
use crate::config::Config;
use crate::distill::Material;
use crate::front;
use crate::git::Checkout;
use crate::keys;
use crate::notify::{self, Notice};
use crate::protocol::{
    Activity, AgentEvent, Answer, Asking, Conversation, Front, SessionInfo, State, TaskInfo,
    TaskOutcome, TaskRecord, TaskResult, TaskSpec, TaskState, TaskView,
};
use crate::spending::Spending;
use crate::state::SavedSession;
use crate::task::Task;
use crate::tasks;
use crate::vt;
use anyhow::{Context, Result, ensure};
use portable_pty::{CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use std::collections::BTreeMap;
use std::io::{self, ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long a stopped session gets to exit after its hang-up before it's
/// killed outright.
pub const STOP_GRACE: Duration = Duration::from_secs(2);

/// Chunks of output a viewer may fall behind by before it's dropped.
const VIEWER_BACKLOG: usize = 256;

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
    /// What the user was last told about the session, while it still
    /// holds: it waits on them, or it's done.
    told: Option<Activity>,
    /// `Some` for a task, whose screen shows what Claude does in its runs
    /// rather than a program in a PTY.
    task: Option<Task>,
    /// What the agent was asked to do, for a session started with
    /// something to do: a task, in a terminal or in the background.
    goal: Option<TaskInfo>,
    /// Whether its agent has been reminded that its task is still open,
    /// which it is once.
    reminded: bool,
    /// Tasks that closed of themselves, like a background task whose run
    /// ended, for the daemon to write down.
    closed: Vec<TaskRecord>,
    /// What has happened to it since the daemon last asked, for it to tell.
    changes: Vec<Change>,
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
    /// A task its run had closed opened again, with a follow-up.
    Reopened,
    /// Its agent's turn ended with its task still open: the task waits on
    /// the user.
    TaskWaiting,
}

impl Session {
    /// Starts `argv` in a PTY of its own. `command` is what was asked for;
    /// `argv` may add to it, like the flags that make an agent report what
    /// it's doing.
    pub fn spawn(
        id: String,
        name: String,
        command: Vec<String>,
        argv: &[String],
        cwd: PathBuf,
        env: &BTreeMap<String, String>,
    ) -> Result<Session> {
        let pty = native_pty_system().openpty(size(24, 80))?;
        let mut builder = CommandBuilder::new(&argv[0]);
        builder.args(&argv[1..]);
        builder.cwd(&cwd);
        builder.env_clear();
        for (key, value) in env {
            builder.env(key, value);
        }
        let mut child = pty.slave.spawn_command(builder)?;
        // Only the child may hold the terminal's other end, so that its exit
        // ends the output.
        drop(pty.slave);

        let output = pty.master.try_clone_reader()?;
        let term = Arc::new(Term::new(Some(Pty {
            input: Mutex::new(pty.master.take_writer()?),
            master: Mutex::new(pty.master),
        })));
        thread::spawn({
            let term = term.clone();
            move || term.pump(output)
        });

        let pid = child.process_id();
        let state = Arc::new(Mutex::new(State::Running));
        let changed = Arc::new(Mutex::new(SystemTime::now()));
        thread::spawn({
            let state = state.clone();
            let changed = changed.clone();
            move || {
                let ended = match child.wait() {
                    Ok(status) => ended(&status),
                    Err(_) => State::Exited { code: 1 },
                };
                *state.lock().unwrap() = ended;
                *changed.lock().unwrap() = SystemTime::now();
            }
        });

        Ok(Session {
            name,
            id,
            command,
            checkout: Checkout::find(&cwd),
            cwd,
            env: env.clone(),
            pid,
            state,
            activity: None,
            changed,
            conversation: None,
            rollouts: None,
            told: None,
            screen_watch: ScreenWatch::default(),
            front: None,
            front_group: None,
            front_checked: Instant::now(),
            task: None,
            goal: None,
            reminded: false,
            closed: Vec::new(),
            changes: Vec::new(),
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
            told: None,
            screen_watch: ScreenWatch::default(),
            front: Some(Front::Task),
            front_group: None,
            front_checked: Instant::now(),
            task: Some(task),
            goal: None,
            reminded: false,
            closed: Vec::new(),
            changes: Vec::new(),
            term,
        }
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

    /// How a task's latest run ended, once it has: `None` while a run is
    /// going on, before any has ended, and for a session that isn't a task.
    pub fn finished_run(&self) -> Option<TaskResult> {
        let task = self.task.as_ref()?;
        if task.is_working() {
            return None;
        }
        task.result()
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

    /// A task started again after a restart: it says so, and waits at rest
    /// for a follow-up, which carries its conversation on.
    pub fn came_back(&mut self) {
        if let Some(task) = &self.task {
            task.note(
                "crystal restarted, and what this task showed before is gone. \
                 `crystal send` carries its conversation on.",
            );
            self.on_agent_event(AgentEvent::Started);
        }
    }

    /// Makes the session a task: its agent was asked to do something, which
    /// stays open until the task is closed done or failed.
    pub fn give_task(&mut self, goal: TaskInfo) {
        self.goal = Some(goal);
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
        })
    }

    /// Where the session runs: the directory its project is found from.
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// The top of the worktree the session runs in, or outside git, where
    /// it runs.
    pub fn checkout_top(&self) -> PathBuf {
        match &self.checkout {
            Some(checkout) => checkout.worktree().path,
            None => self.cwd.clone(),
        }
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

    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            front: self.front.clone(),
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
        }
    }

    /// Works out what the agent is doing from what it just reported. A
    /// turn that ends with the session's task still open is a question for
    /// the user, and the task waits on them until the agent works again.
    pub fn on_agent_event(&mut self, event: AgentEvent) {
        let turn_ended = event == AgentEvent::TurnEnded
            || (event == AgentEvent::StillIdle && self.activity == Some(Activity::Working));
        let mut activity = next_activity(self.activity, event, self.term.is_watched());
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
    /// to tell.
    fn set_activity(&mut self, activity: Option<Activity>) {
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
        match self.task.as_mut().map(Task::events) {
            Some(events) => {
                for event in events {
                    // How a run ended closes its task first: a task that
                    // stays open waits on the user.
                    self.follow_runs(event);
                    self.on_agent_event(event);
                }
            }
            None => self.check_screen(),
        }
        self.fail_task_if_ended();
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
        if !open || self.is_running() || !tasks_on() {
            return;
        }
        let why = format!("its session ended: {}", self.state.lock().unwrap());
        if let Ok(record) = self.close_task(TaskState::Failed, &why) {
            self.closed.push(record);
        }
    }

    /// Looks at what's in front in the terminal, when its job has changed
    /// or it's been a while. Cheap otherwise: one question to the terminal.
    pub fn check_front(&mut self) {
        if self.task.is_some() || !self.is_running() {
            return;
        }
        let Some(group) = self.term.foreground_group() else {
            return;
        };
        let same_job = self.front_group == Some(group);
        if same_job && self.front_checked.elapsed() < FRONT_RECHECK {
            return;
        }
        self.front_group = Some(group);
        self.front_checked = Instant::now();
        if let Some(front) = front::of_process(group) {
            self.set_front(front);
        }
    }

    /// Takes what's in front now. An agent that leaves the front takes what
    /// it was doing with it: the shell it gives the terminal back to isn't
    /// working or waiting on anyone.
    fn set_front(&mut self, front: Front) {
        if self.front.as_ref() == Some(&front) {
            return;
        }
        let agent_left = self.front.as_ref().is_some_and(Front::is_agent);
        if agent_left && self.activity.is_some() {
            self.set_activity(None);
            *self.changed.lock().unwrap() = SystemTime::now();
        }
        self.screen_watch = ScreenWatch::default();
        self.front = Some(front);
    }

    /// Whether an agent is in front in the terminal, the only time its
    /// screen says anything about what an agent is doing.
    fn agent_in_front(&self) -> bool {
        self.front.as_ref().is_some_and(Front::is_agent)
    }

    /// Reads what the agent is doing off the screen, and takes it as an
    /// event when that has changed. Only while an agent is in front: a
    /// shell or any other program can print an agent's words.
    fn check_screen(&mut self) {
        if !self.is_running() || !self.agent_in_front() {
            return;
        }
        let looks = self.term.looks();
        if let Some(event) = self.screen_watch.update(looks) {
            self.on_agent_event(event);
        }
    }

    /// Something to tell the user, when the session has just come to need
    /// them: its agent is asking them something, or is done with a turn
    /// nobody watched.
    pub fn notice(&mut self) -> Option<Notice> {
        let now = self.activity;
        let watched = self.term.is_watched();
        let tell = self.is_running() && notify::worth_telling(now, self.told, watched);
        // Kept even when the user isn't told, say because they were
        // watching: they've seen it, so it isn't news later either.
        self.told = if notify::needs_user(now) { now } else { None };
        match now {
            Some(activity) if tell => Some(Notice::about(&self.info(), activity)),
            _ => None,
        }
    }

    pub fn set_conversation(&mut self, conversation: Conversation) {
        self.conversation = Some(conversation);
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
    /// runs. A program that has ended stays ended.
    pub fn saved(&self) -> Option<SavedSession> {
        if self.is_running() {
            Some(self.launch())
        } else {
            None
        }
    }

    /// What it takes to start the session's program again: its name,
    /// command and directory, and the agent's conversation to pick up.
    pub fn launch(&self) -> SavedSession {
        // A task's conversation comes from Claude's own events, which need
        // no transcript file to resume it.
        let conversation = match &self.task {
            Some(task) => task.conversation().map(|id| Conversation {
                id,
                transcript: None,
            }),
            None => self.conversation.clone(),
        };
        SavedSession {
            name: self.name.clone(),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            conversation,
            task: self.task.as_ref().map(|task| task.spec().clone()),
            goal: self.goal.clone(),
        }
    }

    /// Someone has just looked at the session.
    pub fn seen(&mut self) {
        if self.activity == Some(Activity::Done) {
            self.set_activity(Some(Activity::Idle));
        }
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
}

/// The daemon's side of a PTY: how the program's terminal is sized, and
/// the way in.
struct Pty {
    master: Mutex<Box<dyn MasterPty + Send>>,
    input: Mutex<Box<dyn Write + Send>>,
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
}

struct Viewer {
    id: u64,
    feed: SyncSender<Arc<[u8]>>,
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
    /// A screen of 24 rows by 80 columns, until a viewer gives it another
    /// size.
    fn new(pty: Option<Pty>) -> Term {
        Term {
            pty,
            screen: Mutex::new(Screen {
                vt: vt::Screen::answering(24, 80),
                viewers: Vec::new(),
                listeners: Vec::new(),
                ended: false,
            }),
        }
    }

    /// A screen with no program behind it, for a task to draw on.
    pub fn without_terminal() -> Term {
        Term::new(None)
    }

    /// Starts showing the session to a new viewer. With `with_history`, the
    /// viewer's screen gets the history too, so that it can scroll back
    /// through output from before it came.
    pub fn watch(&self, with_history: bool) -> Watch {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut screen = self.screen.lock().unwrap();
        let snapshot = screen.vt.state_formatted(with_history);
        let feed = (!screen.ended).then(|| {
            let (feed, rx) = mpsc::sync_channel(VIEWER_BACKLOG);
            screen.viewers.push(Viewer { id, feed });
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

    /// The screen, one string per row, after the last `history` rows of
    /// the history.
    pub fn recent_rows(&self, history: usize) -> Vec<String> {
        self.screen.lock().unwrap().vt.recent_rows(history)
    }

    /// What the screen says the agent is doing.
    pub fn looks(&self) -> Looks {
        let screen = self.screen.lock().unwrap();
        agent_screen::read(&screen.vt.rows(false), &screen.vt.title())
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

    /// The process group in front in the terminal: the job its keys go to.
    /// `None` without a terminal, or when the terminal won't say.
    pub fn foreground_group(&self) -> Option<i32> {
        let pty = self.pty.as_ref()?;
        let master = pty.master.lock().unwrap();
        master.process_group_leader()
    }

    pub fn is_watched(&self) -> bool {
        !self.screen.lock().unwrap().viewers.is_empty()
    }

    pub fn unwatch(&self, id: u64) {
        let mut screen = self.screen.lock().unwrap();
        screen.viewers.retain(|viewer| viewer.id != id);
    }

    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        match &self.pty {
            Some(pty) => pty.input.lock().unwrap().write_all(bytes),
            None => Err(io::Error::other("a task takes no keys")),
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let mut screen = self.screen.lock().unwrap();
        screen.vt.resize(rows, cols);
        if let Some(pty) = &self.pty {
            pty.master.lock().unwrap().resize(size(rows, cols))?;
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
    /// the program's questions to its terminal.
    fn pump(&self, mut output: Box<dyn Read + Send>) {
        let mut buf = [0; 16 * 1024];
        loop {
            let n = match output.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let replies = self.take_output(&buf[..n]);
            if !replies.is_empty() {
                let _ = self.write(&replies);
            }
        }
        self.close();
    }

    /// Keeps the screen up to date with `output` and passes it on to every
    /// viewer. Returns what the program asked its terminal, to answer.
    fn take_output(&self, output: &[u8]) -> Vec<u8> {
        let mut screen = self.screen.lock().unwrap();
        screen.vt.process(output);
        // Viewers get the same output, so that their own screens keep the
        // same history.
        let chunk: Arc<[u8]> = output.into();
        // A viewer that's gone, or too far behind to catch up, is dropped
        // rather than holding up the program.
        screen
            .viewers
            .retain(|viewer| viewer.feed.try_send(chunk.clone()).is_ok());
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
    };
    // A turn that ends while someone's watching has been seen.
    if after == Activity::Done && watched {
        Some(Activity::Idle)
    } else {
        Some(after)
    }
}

/// How `ls` shows a task's command: the `claude -p` it runs, with its own
/// arguments after the prompt.
fn task_command(spec: &TaskSpec) -> Vec<String> {
    let mut command = vec!["claude".to_string(), "-p".to_string(), spec.prompt.clone()];
    command.extend(spec.args.iter().cloned());
    command
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
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
}
