//! A program running in a PTY of its own, or a task: Claude Code run
//! without a terminal.

use crate::agent_screen::{self, Looks, ScreenWatch};
use crate::codex::Rollouts;
use crate::front;
use crate::git::Checkout;
use crate::history::{self, HISTORY_LINES, HistoryKeeper};
use crate::notify::{self, Notice};
use crate::protocol::{
    Activity, AgentEvent, Conversation, Front, SessionInfo, State, TaskInfo, TaskOutcome,
    TaskRecord, TaskResult, TaskSpec,
};
use crate::state::SavedSession;
use crate::task::Task;
use anyhow::{Context, Result, ensure};
use portable_pty::{CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use std::collections::BTreeMap;
use std::io::{self, ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
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
    /// Tasks that closed of themselves, like a background task whose run
    /// ended, for the daemon to write down.
    closed: Vec<TaskRecord>,
    term: Arc<Term>,
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
            closed: Vec::new(),
            term,
        })
    }

    /// Makes a task: a session for `claude -p` runs of `spec`, whose screen
    /// shows what Claude does. It starts at rest; [`Session::prompt`] gives
    /// it its prompt. Given a `conversation`, its runs carry it on.
    pub fn task(
        id: String,
        name: String,
        spec: TaskSpec,
        cwd: PathBuf,
        env: BTreeMap<String, String>,
        conversation: Option<String>,
    ) -> Session {
        let term = Arc::new(Term::without_terminal());
        let state = Arc::new(Mutex::new(State::Running));
        let command = task_command(&spec);
        let task = Task::new(
            spec,
            cwd.clone(),
            env,
            term.clone(),
            state.clone(),
            conversation,
        );
        Session {
            name,
            id,
            command,
            checkout: Checkout::find(&cwd),
            cwd,
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
            closed: Vec::new(),
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
            task.pid().is_none(),
            "{name} is still working: `crystal wait {name}` for it first"
        );
        task.result()
            .with_context(|| format!("{name} has no answer yet"))
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

    /// Closes the session's task, done or failed, saying how it went, and
    /// gives back the task as the project's history keeps it.
    pub fn close_task(&mut self, failed: bool, summary: &str) -> Result<TaskRecord> {
        let name = &self.name;
        let goal = self.goal.as_mut().with_context(|| {
            format!("{name} has no task: it wasn't started with something to do")
        })?;
        goal.outcome = Some(TaskOutcome {
            failed,
            summary: summary.trim().to_string(),
            closed: seconds_since_epoch(SystemTime::now()),
        });
        *self.changed.lock().unwrap() = SystemTime::now();
        Ok(self.task_record().expect("the task was just closed"))
    }

    /// The tasks that have closed of themselves since this was last asked,
    /// for the daemon to write down.
    pub fn take_closed(&mut self) -> Vec<TaskRecord> {
        std::mem::take(&mut self.closed)
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
            goal: goal.goal.clone(),
            session: self.name.clone(),
            project,
            branch: worktree.and_then(|worktree| worktree.branch),
            background: goal.background,
            backlog: goal.backlog,
            outcome: goal.outcome.clone(),
        })
    }

    /// Where the session runs: the directory its project is found from.
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    pub fn is_running(&self) -> bool {
        *self.state.lock().unwrap() == State::Running
    }

    /// What the session's agent is doing, when it reports that.
    pub fn activity(&self) -> Option<Activity> {
        self.activity
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
        }
    }

    /// Works out what the agent is doing from what it just reported.
    pub fn on_agent_event(&mut self, event: AgentEvent) {
        let activity = next_activity(self.activity, event, self.term.is_watched());
        if activity != self.activity {
            self.activity = activity;
            *self.changed.lock().unwrap() = SystemTime::now();
        }
    }

    /// Keeps up with what the session's agent is doing: a task's from its
    /// runs, any other program's from its screen.
    pub fn check(&mut self) {
        let Some(task) = &mut self.task else {
            return self.check_screen();
        };
        let events = task.events();
        for event in events {
            self.on_agent_event(event);
            self.follow_runs(event);
        }
    }

    /// A background task closes itself when a run ends, from what Claude
    /// said at the end, and opens again when a follow-up starts another.
    fn follow_runs(&mut self, event: AgentEvent) {
        let Some(goal) = &mut self.goal else {
            return;
        };
        match event {
            AgentEvent::TurnStarted if goal.outcome.is_some() => goal.outcome = None,
            AgentEvent::TurnEnded if goal.outcome.is_none() => {
                let Some(result) = self.task.as_ref().and_then(Task::result) else {
                    return;
                };
                let summary = result.text.lines().next().unwrap_or("").to_string();
                if let Ok(record) = self.close_task(result.failed, &summary) {
                    self.closed.push(record);
                }
            }
            _ => {}
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
            self.activity = None;
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
            self.activity = Some(Activity::Idle);
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
    parser: vt100::Parser<Callbacks>,
    /// Sees that rows scrolling up off the screen reach the parser's
    /// history.
    history: HistoryKeeper,
    viewers: Vec<Viewer>,
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
        let parser = vt100::Parser::new_with_callbacks(24, 80, HISTORY_LINES, Callbacks::default());
        Term {
            pty,
            screen: Mutex::new(Screen {
                parser,
                history: HistoryKeeper::default(),
                viewers: Vec::new(),
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
        let mut snapshot = Vec::new();
        if with_history {
            snapshot = history::replay(screen.parser.screen_mut());
        }
        snapshot.extend(screen.parser.screen().state_formatted());
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

    /// What the screen says the agent is doing.
    pub fn looks(&self) -> Looks {
        let screen = self.screen.lock().unwrap();
        agent_screen::read(screen.parser.screen(), &screen.parser.callbacks().title)
    }

    /// Whether the program has asked the arrow keys to send `ESC O` rather
    /// than `ESC [`, which changes what a key named to `send-keys` sends.
    pub fn wants_application_cursor(&self) -> bool {
        self.screen
            .lock()
            .unwrap()
            .parser
            .screen()
            .application_cursor()
    }

    /// Whether the program has asked for pastes to be marked as pastes.
    pub fn wants_bracketed_paste(&self) -> bool {
        self.screen
            .lock()
            .unwrap()
            .parser
            .screen()
            .bracketed_paste()
    }

    /// What's on the screen, one string per row, after the rows of the
    /// history with `with_history`.
    pub fn rows(&self, with_history: bool) -> Vec<String> {
        let mut screen = self.screen.lock().unwrap();
        let mut rows = Vec::new();
        if with_history {
            rows = history::text(screen.parser.screen_mut());
        }
        let (_, cols) = screen.parser.screen().size();
        rows.extend(screen.parser.screen().rows(0, cols));
        rows
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
        let (old_rows, _) = screen.parser.screen().size();
        screen.history.resize(old_rows, rows);
        screen.parser.screen_mut().set_size(rows, cols);
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
        let mut guard = self.screen.lock().unwrap();
        let screen = &mut *guard;
        // Viewers get what the screen was fed, so that their own screens
        // keep the same history.
        let chunk: Arc<[u8]> = screen.history.feed(&mut screen.parser, output).into();
        // A viewer that's gone, or too far behind to catch up, is dropped
        // rather than holding up the program.
        screen
            .viewers
            .retain(|viewer| viewer.feed.try_send(chunk.clone()).is_ok());
        std::mem::take(&mut screen.parser.callbacks_mut().replies)
    }
}

/// What vt100 hands back to us as it reads a program's output: questions
/// the program asks its terminal, and the title it gives it.
#[derive(Default)]
struct Callbacks {
    /// Answers to send back: where the cursor is, and what kind of terminal
    /// this is. Viewers only draw, so the answers come from here, whether
    /// anyone's watching or not.
    replies: Vec<u8>,
    /// Agents put a spinner here while they work.
    title: String,
}

impl vt100::Callbacks for Callbacks {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = String::from_utf8_lossy(title).into_owned();
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let param = params.first().and_then(|param| param.first()).copied();
        match (i1, c, param.unwrap_or(0)) {
            // Device status.
            (None, 'n', 5) => self.replies.extend_from_slice(b"\x1b[0n"),
            // Cursor position, 1-based.
            (None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                let _ = write!(self.replies, "\x1b[{};{}R", row + 1, col + 1);
            }
            // Primary device attributes: a VT100 with advanced video.
            (None, 'c', 0) => self.replies.extend_from_slice(b"\x1b[?1;2c"),
            // Secondary device attributes.
            (Some(b'>'), 'c', 0) => self.replies.extend_from_slice(b"\x1b[>0;0;0c"),
            _ => {}
        }
    }
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
