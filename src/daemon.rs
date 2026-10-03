//! The background process that owns every session. It outlives the
//! terminal it was started from, so sessions keep running when the
//! client goes away.

use crate::agents;
use crate::artifacts;
use crate::backlog;
use crate::catalog;
use crate::codex;
use crate::config::{Config, MemorySettings};
use crate::db::Db;
use crate::distill::{self, Job};
use crate::embed;
use crate::env;
use crate::event_log::{self, Bus, Subscription};
use crate::events::{Event, Filter, Kind, Since};
use crate::flow_run::{self, Ended, FlowRun, Next, Place, RunState, StepState};
use crate::flows;
use crate::front;
use crate::git;
use crate::handoff;
use crate::mcp;
use crate::memory::{self, Added};
use crate::names;
use crate::notify::{self, Notice};
use crate::plugin_hooks;
use crate::project;
use crate::protocol::{
    self, Activity, AgentEvent, Artifact, ArtifactKind, Backlog, Conversation, Frame, Front,
    NewSession, NewTask, PendingTask, Request, Response, SessionInfo, State, TaskInfo, TaskOutcome,
    TaskRecord, TaskSpec, TaskStart, TaskState, TaskView,
};
use crate::report;
use crate::session::{Change, STOP_GRACE, Session, Term};
use crate::skill;
use crate::socket;
use crate::spending::Spending;
use crate::state::{self, SavedSession};
use crate::tasks;
use crate::typing;
use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use std::collections::{BTreeMap, HashSet};
use std::io::{BufReader, ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{fs, process, thread};

/// How often the daemon reads every session's screen for what its agent is
/// doing, tells the user about the sessions that need them, and writes
/// down the sessions that are running.
const KEEP_UP_EVERY: Duration = Duration::from_millis(250);

/// How often a client that only listens is checked for having hung up.
const LOOK_FOR_HANG_UP: Duration = Duration::from_secs(1);

/// How many rows of a session's history, above its screen, `wait --output`
/// looks through: enough for what scrolled off between two looks.
const HISTORY_MATCHED: usize = 200;

/// The least time between two looks at a screen for `wait --output`, so a
/// program writing without a break doesn't keep the daemon looking.
const LOOK_AT_MOST_EVERY: Duration = Duration::from_millis(50);

pub fn run(socket: &Path) -> Result<()> {
    // Leave the client's terminal, so closing it doesn't hang up the
    // daemon. Fails harmlessly when run in the foreground from a shell.
    // SAFETY: setsid has no preconditions.
    unsafe {
        libc::setsid();
    }
    // Before listening: a daemon that can't keep its state doesn't start,
    // rather than run with none and write over it.
    let db = Db::open(socket)?;
    let listener = listen(socket)?;
    let events = Arc::new(Bus::new(socket));
    let hooks = plugin_hooks::follow(&events, socket);
    thread::spawn({
        let events = events.clone();
        move || {
            loop {
                events.prune(settings().events.keep_days);
                thread::sleep(event_log::PRUNE_EVERY);
            }
        }
    });
    let daemon = Arc::new(Daemon {
        socket: socket.to_path_buf(),
        db: Mutex::new(db),
        sessions: Mutex::default(),
        flows: Mutex::default(),
        spending: Arc::new(Spending::new(Db::open(socket)?)),
        events,
        distilling: Arc::default(),
        preparing: Arc::default(),
        handoff: Mutex::default(),
    });
    // A daemon starts again after every upgrade, so this is where the skill
    // an earlier crystal installed learns this one's commands: before the
    // saved sessions start, so that Claude Code in them reads the new one.
    match skill::refresh() {
        Ok(Some(path)) => eprintln!("crystal daemon: updated the skill in {}", path.display()),
        Ok(None) => {}
        Err(err) => eprintln!("crystal daemon: couldn't update the skill: {err:#}"),
    }
    daemon.start_saved_sessions();
    daemon.take_up_flows();
    hooks.start_up();
    // With search by meaning on, the model is loaded and every entry
    // without a vector given one now, rather than when a session starts.
    thread::spawn({
        let socket = socket.to_path_buf();
        move || embed_waiting(&socket)
    });
    thread::spawn({
        let daemon = daemon.clone();
        move || daemon.keep_up()
    });
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let daemon = daemon.clone();
        thread::spawn(move || {
            if let Err(err) = daemon.serve(conn) {
                eprintln!("crystal daemon: {err:#}");
            }
        });
    }
    Ok(())
}

fn listen(socket: &Path) -> Result<UnixListener> {
    socket::prepare_dir(socket)?;
    match UnixListener::bind(socket) {
        Ok(listener) => Ok(listener),
        Err(err) if err.kind() == ErrorKind::AddrInUse => {
            if UnixStream::connect(socket).is_ok() {
                bail!("a daemon is already listening on {}", socket.display());
            }
            // Left behind by a daemon that didn't shut down cleanly.
            fs::remove_file(socket)?;
            Ok(UnixListener::bind(socket)?)
        }
        Err(err) => Err(err).with_context(|| format!("couldn't listen on {}", socket.display())),
    }
}

struct Daemon {
    socket: PathBuf,
    /// Where the running sessions and the flow runs are written down, to
    /// start them again after a restart, and each project's backlog and
    /// task history. Whoever needs it and `sessions` or `flows` locks those
    /// first, and this last.
    db: Mutex<Db>,
    /// In the order they were created, which is the order `ls` shows.
    sessions: Mutex<Vec<Session>>,
    /// Every flow run, the oldest first. A run's steps are sessions, so
    /// whoever needs both locks `sessions` first, then this, and never the
    /// other way round, which could leave two threads each waiting on the
    /// other.
    flows: Mutex<Vec<FlowRun>>,
    /// What background tasks have spent today, on a connection to the
    /// database of its own: each task's runs add to it as they end.
    spending: Arc<Spending>,
    /// What happens goes through here: into the event log, and on to the
    /// clients and plugins that listen.
    events: Arc<Bus>,
    /// The sessions the distiller is reading now, by id: one pass at a
    /// time over each.
    distilling: Arc<Mutex<HashSet<String>>>,
    /// Getting the model that searches memory by meaning ready: what's
    /// being done, and why it last failed.
    preparing: Arc<Mutex<Preparing>>,
    /// Held while a worktree's handoff file is written: each write reads
    /// the file, adds to it and writes it back. Never held while taking
    /// another lock.
    handoff: Mutex<()>,
}

/// What [`Daemon::prepare_embeddings`] is doing, or why it failed.
#[derive(Debug, Default)]
struct Preparing {
    doing: Option<&'static str>,
    failed: Option<String>,
}

impl Daemon {
    fn serve(&self, conn: UnixStream) -> Result<()> {
        let mut input = BufReader::new(&conn);
        let Some(incoming) = protocol::recv_request(&mut input)? else {
            return Ok(());
        };
        // A shutdown goes through whatever the versions: it's how a crystal
        // of another version gets this daemon out of its way.
        let ours = protocol::version();
        if incoming.version.as_deref() != Some(ours.as_str()) && !incoming.is_shutdown() {
            let crystal = socket::crystal_for(&self.socket);
            let message = version_mismatch(&ours, incoming.version.as_deref(), &crystal);
            return Ok(protocol::send(&conn, &Response::Error { message })?);
        }
        let request = incoming.request()?;
        // These take the connection over, and answer as they go.
        match request {
            Request::Attach {
                name,
                rows,
                cols,
                history,
            } => {
                return match self.find(name.as_deref()) {
                    Ok(found) => attach(&conn, input, found, (rows, cols), history),
                    Err(err) => Ok(protocol::send(&conn, &Response::from(err))?),
                };
            }
            Request::Subscribe { filter, since } => {
                return self.stream_events(&conn, filter, since);
            }
            Request::WaitOutput {
                name,
                pattern,
                timeout_ms,
            } => {
                let timeout = timeout_ms.map(Duration::from_millis);
                return self.wait_for_output(&conn, &name, &pattern, timeout);
            }
            _ => {}
        }
        let shutdown = matches!(request, Request::Shutdown { .. });
        let response = self.handle(request).unwrap_or_else(Response::from);
        protocol::send(&conn, &response)?;
        if shutdown {
            let _ = fs::remove_file(&self.socket);
            process::exit(0);
        }
        Ok(())
    }

    /// Starts again the sessions that were running when the last daemon
    /// stopped without being asked to: it crashed, or the machine rebooted.
    fn start_saved_sessions(&self) {
        let mut sessions = self.sessions.lock().unwrap();
        let saved = self.db.lock().unwrap().sessions();
        let saved = saved.unwrap_or_else(|err| {
            eprintln!("crystal daemon: couldn't read the sessions to start again: {err:#}");
            Vec::new()
        });
        for saved in saved {
            let goal = saved.goal.clone();
            let backlog = goal.as_ref().and_then(|goal| goal.backlog);
            let started = match saved.task {
                // A task comes back at rest: a run it was in the middle of
                // can't be picked up halfway, so it isn't run again either.
                Some(spec) => {
                    let task = NewTask {
                        name: Some(saved.name.clone()),
                        cwd: saved.cwd,
                        spec,
                        env: env::current(),
                        backlog,
                    };
                    let conversation = saved.conversation.map(|conversation| conversation.id);
                    start_task(
                        &mut sessions,
                        &self.socket,
                        &self.spending,
                        task,
                        conversation,
                        false,
                    )
                }
                None => {
                    let new = NewSession {
                        name: Some(saved.name.clone()),
                        cwd: saved.cwd,
                        command: saved.command,
                        env: env::current(),
                        task: goal.as_ref().map(|goal| goal.goal.clone()),
                        backlog,
                    };
                    start(
                        &mut sessions,
                        &self.socket,
                        new,
                        saved.conversation,
                        saved.resume,
                    )
                }
            };
            match started {
                // It comes back with its task as it was, closed or not.
                Ok(_) => {
                    if let (Some(goal), Some(session)) = (goal, sessions.last_mut()) {
                        session.give_task(goal);
                    }
                }
                Err(err) => eprintln!(
                    "crystal daemon: couldn't start {} again: {err:#}",
                    saved.name
                ),
            }
        }
    }

    /// Again and again: reads every session's screen for what its agent is
    /// doing, tells what has changed, and writes down the running sessions
    /// when they've changed.
    fn keep_up(&self) {
        let mut last_saved: Vec<SavedSession> = Vec::new();
        // The sessions whose program has ended and been told of, by id.
        let mut told_ended: HashSet<String> = HashSet::new();
        let mut last_runs: Vec<FlowRun> = Vec::new();
        loop {
            thread::sleep(KEEP_UP_EVERY);
            let mut sessions = self.sessions.lock().unwrap();
            self.number_tasks(&mut sessions);
            let claimed: Vec<String> = sessions
                .iter()
                .filter_map(|session| session.conversation_id().map(String::from))
                .collect();
            let claimed: Vec<&str> = claimed.iter().map(String::as_str).collect();
            let looking: Vec<(PathBuf, SystemTime)> = sessions
                .iter()
                .filter_map(Session::looking_for_conversation)
                .map(|rollouts| (rollouts.cwd().to_path_buf(), rollouts.started()))
                .collect();
            for session in sessions.iter_mut() {
                session.find_conversation(&claimed, &looking);
                session.check_front();
                session.check();
                self.tell_changes(session);
                for closed in session.take_closed() {
                    self.write_down_closed(session, &closed);
                }
            }
            // Before telling the user anything: a step the flow goes on
            // from needs nobody, and a gate needs them.
            self.follow_flows(&mut sessions);
            for session in sessions.iter_mut() {
                if let Some(notice) = session.notice() {
                    notify::tell(notice);
                }
                self.tell_changes(session);
                if !session.is_running() && told_ended.insert(session.id.clone()) {
                    let info = session.info();
                    let status = info.state.to_string();
                    self.events.emit(Event::ended(&info, status));
                }
            }
            told_ended.retain(|id| sessions.iter().any(|session| &session.id == id));
            // Written while the list is still locked, so that an older list
            // can never be written after a shutdown has emptied it.
            let saved: Vec<SavedSession> = sessions.iter().filter_map(Session::saved).collect();
            if saved != last_saved {
                match self.db.lock().unwrap().save_sessions(&saved) {
                    Ok(()) => last_saved = saved,
                    Err(err) => eprintln!("crystal daemon: couldn't save the sessions: {err:#}"),
                }
            }
            let runs = self.flows.lock().unwrap().clone();
            if runs != last_runs {
                match self.db.lock().unwrap().save_flow_runs(&runs) {
                    Ok(()) => last_runs = runs,
                    Err(err) => eprintln!("crystal daemon: couldn't save the flow runs: {err:#}"),
                }
            }
        }
    }

    /// Takes up the flow runs the last daemon wrote down. A step that was
    /// running then was cut short, and waits to be run again; one waiting
    /// at its gate waits on the user again. Their steps start from this
    /// daemon's environment, as the sessions it starts again do.
    fn take_up_flows(&self) {
        let mut sessions = self.sessions.lock().unwrap();
        let runs = self.db.lock().unwrap().flow_runs();
        let mut runs = runs.unwrap_or_else(|err| {
            eprintln!("crystal daemon: couldn't read the flow runs: {err:#}");
            Vec::new()
        });
        for run in &mut runs {
            // A step in a terminal whose session came back with its task
            // open carries on there; any other was cut short.
            let carried_on = run.running().is_some_and(|step| {
                run.in_terminal(step)
                    && step_session(&mut sessions, run, step).is_some_and(|session| {
                        let open = session
                            .task_record()
                            .is_some_and(|task| task.outcome.is_none());
                        session.is_running() && open
                    })
            });
            if !carried_on {
                run.interrupt();
            }
            run.env = env::current();
            let at_gate = run
                .current()
                .filter(|&step| run.steps[step].state == StepState::AtGate);
            if let Some(session) = at_gate.and_then(|step| step_session(&mut sessions, run, step)) {
                session.on_agent_event(AgentEvent::Asking);
            }
        }
        *self.flows.lock().unwrap() = runs;
    }

    /// Keeps each flow run going: once the task of the step running has
    /// finished its run, the run takes how it went and does what comes
    /// next.
    fn follow_flows(&self, sessions: &mut Vec<Session>) {
        let mut runs = self.flows.lock().unwrap();
        for run in runs.iter_mut() {
            let Some(step) = run.running() else {
                continue;
            };
            let Some(mut ended) = how_step_ended(sessions, run, step) else {
                continue;
            };
            ended.artifacts = self.kept_files(run.steps[step].task);
            let cost_usd = ended.cost_usd;
            let next = run.step_ended(step, ended);
            self.events.emit(Event::step_ended(run, step, cost_usd));
            // The flow has taken the step's answer on to the next step:
            // nobody needs to look at it to know it's done.
            if matches!(next, Next::Run { .. })
                && let Some(session) = step_session(sessions, run, step)
            {
                session.seen();
            }
            self.carry_out(sessions, run, next);
            // A run that has just stopped needs the user as much as a gate
            // does, but its step's session has ended and can't say so.
            if run.state() == RunState::Failed {
                notify::tell(Notice {
                    session: run.steps[step].session.clone().unwrap_or_default(),
                    activity: Activity::Waiting,
                    text: format!("{} failed at {}", run.name, run.step_name(step)),
                });
            }
        }
    }

    /// Does what a flow run needs once it has changed: runs its next step,
    /// or has the session of the step at its gate wait on the user; and
    /// tells of it.
    fn carry_out(&self, sessions: &mut Vec<Session>, run: &mut FlowRun, next: Next) {
        match next {
            Next::Run { step, prompt } => match self.start_step(sessions, run, step, &prompt) {
                Ok(()) => {
                    let info = step_session(sessions, run, step).map(|session| session.info());
                    self.events
                        .emit(Event::step_started(run, step, info.as_ref()));
                }
                Err(err) => {
                    run.could_not_start(step, format!("{err:#}"));
                    self.events.emit(Event::flow_ended(run));
                }
            },
            Next::Gate(step) => {
                if let Some(session) = step_session(sessions, run, step) {
                    session.on_agent_event(AgentEvent::Asking);
                }
                self.events.emit(Event::gate(run, step));
            }
            Next::Finished | Next::Stopped => self.events.emit(Event::flow_ended(run)),
        }
    }

    /// Runs `step` of `run`, asking it `prompt`, where the flow places it.
    /// A step in the background whose session is there at rest takes it as
    /// a follow-up, in the same conversation; an ended one makes way for a
    /// new task, which carries its conversation on. A step in a terminal
    /// starts afresh in a session of its own, its ended one making way.
    /// With neither, the step starts in a new task of its own.
    fn start_step(
        &self,
        sessions: &mut Vec<Session>,
        run: &mut FlowRun,
        step: usize,
        prompt: &str,
    ) -> Result<()> {
        let terminal = run.in_terminal(step);
        // A step in a terminal goes on once its task closes.
        ensure!(
            !terminal || tasks::enabled(&settings()),
            "step {} runs in a terminal, which takes tasks: {}",
            run.step_name(step),
            crate::plugins::off("tasks")
        );
        let mut conversation = None;
        let had = run.steps[step].session.as_ref();
        if let Some(index) = had.and_then(|name| sessions.iter().position(|s| s.name == *name)) {
            let session = &sessions[index];
            match (session.is_task(), session.is_running()) {
                (true, true) => return session.prompt(prompt),
                (true, false) => {
                    conversation = session.launch().conversation.map(|found| found.id);
                    sessions.remove(index);
                }
                (false, false) if terminal => {
                    sessions.remove(index);
                }
                // The step's agent still there to read, or a session that
                // has taken the name since.
                (false, _) => {}
            }
        }
        let cwd = self.step_dir(run, step)?;
        let taken = |name: &str| sessions.iter().any(|session| session.name == name);
        let name = unique_name(&run.session_name(step), taken);
        let name = if terminal {
            let (command, asked) = run.command(step, prompt);
            let new = NewSession {
                name: Some(name),
                cwd: cwd.clone(),
                command,
                env: run.env.clone(),
                task: Some(asked),
                backlog: None,
            };
            start(sessions, &self.socket, new, None, None)?
        } else {
            let task = NewTask {
                name: Some(name),
                cwd: cwd.clone(),
                spec: run.task_spec(step, prompt),
                env: run.env.clone(),
                backlog: None,
            };
            start_task(
                sessions,
                &self.socket,
                &self.spending,
                task,
                conversation,
                true,
            )?
        };
        // As a task, it's the step, in the project's history and memory,
        // rather than the whole of its prompt.
        let session = sessions.last_mut().expect("it was just started");
        if session.task_record().is_some() {
            let goal = format!("{} {}: {}", run.name, run.step_name(step), run.goal);
            session.give_task(new_task_info(goal, !terminal, None));
            self.number_tasks(std::slice::from_mut(session));
        }
        let task = session.task_id();
        self.tell_started(sessions, &name, Kind::TaskOpened);
        let ran = &mut run.steps[step];
        ran.session = Some(name);
        ran.cwd = Some(cwd);
        ran.task = task;
        Ok(())
    }

    /// Where `step` of `run` runs, as [`FlowRun::place`] says: the
    /// worktree the run makes for itself is made the first time a step
    /// asks for it.
    fn step_dir(&self, run: &mut FlowRun, step: usize) -> Result<PathBuf> {
        match run.place(step) {
            Place::In(dir) => Ok(dir),
            Place::NewWorktree => {
                let (worktree, branch) = git::add_new_worktree(&run.cwd, &run.slug())?;
                self.events
                    .emit(Event::worktree(true, &worktree, Some(&branch)));
                run.worktree = Some(worktree.clone());
                Ok(worktree)
            }
        }
    }

    /// Starts a run of the flow called `flow` on `goal`: the project's own
    /// flow of that name, or the config file's.
    fn start_flow(
        &self,
        flow: &str,
        goal: String,
        cwd: PathBuf,
        env: BTreeMap<String, String>,
    ) -> Result<String> {
        let config = settings();
        flows::ensure_enabled(&config)?;
        let found = flows::find(&config, &cwd, flow)?;
        let mut sessions = self.sessions.lock().unwrap();
        let mut runs = self.flows.lock().unwrap();
        let name = flow_run::new_name(flow, &runs);
        let mut run = FlowRun::new(
            name.clone(),
            found,
            &config.profiles,
            goal,
            cwd,
            env,
            now_seconds(),
        );
        self.events.emit(Event::flow_started(&run));
        let next = run.start();
        self.carry_out(&mut sessions, &mut run, next);
        runs.push(run);
        flow_run::forget_old(&mut runs);
        Ok(name)
    }

    /// Changes the run called `name` with `change`, then does what that
    /// leads to. A change that answers the gate the run waits at goes on,
    /// or with `sent_back`, the notes it's sent back with, goes back.
    fn change_flow(
        &self,
        name: &str,
        sent_back: Option<&str>,
        change: impl FnOnce(&mut FlowRun) -> Result<Next>,
    ) -> Result<()> {
        flows::ensure_enabled(&settings())?;
        let mut sessions = self.sessions.lock().unwrap();
        let mut runs = self.flows.lock().unwrap();
        let run = runs.iter_mut().find(|run| run.name == name);
        let run = run.with_context(|| format!("there's no flow run called {name}"))?;
        // The step at its gate stops waiting on the user, whatever they
        // said.
        let gate = run
            .current()
            .filter(|&step| run.steps[step].state == StepState::AtGate);
        let next = change(run)?;
        if let Some(step) = gate {
            self.events.emit(Event::gate_answered(run, step, sent_back));
        }
        if let Some(session) = gate.and_then(|step| step_session(&mut sessions, run, step)) {
            session.on_agent_event(AgentEvent::Started);
        }
        self.carry_out(&mut sessions, run, next);
        Ok(())
    }

    /// Cancels the run called `name`: the task of the step it's at, while
    /// it's open, is cancelled and its session stopped, and the run goes no
    /// further.
    fn cancel_flow(&self, name: &str) -> Result<()> {
        flows::ensure_enabled(&settings())?;
        let mut sessions = self.sessions.lock().unwrap();
        let mut runs = self.flows.lock().unwrap();
        let run = runs.iter_mut().find(|run| run.name == name);
        let run = run.with_context(|| format!("there's no flow run called {name}"))?;
        let at_gate = run.state() == RunState::AtGate;
        let step = run.cancel()?;
        if let Some(session) = step_session(&mut sessions, run, step) {
            if at_gate {
                session.on_agent_event(AgentEvent::Started);
            }
            let cancelled = tasks::enabled(&settings())
                .then(|| session.cancel_task("its flow was cancelled"))
                .flatten();
            if let Some(cancelled) = cancelled {
                self.write_down_closed(session, &cancelled);
                session.stop();
            }
        }
        self.events.emit(Event::flow_ended(run));
        Ok(())
    }

    /// Gives every task that has no number yet one: a task just made, or
    /// one from before tasks were numbered. One the database can't number
    /// now is numbered the next time round.
    fn number_tasks(&self, sessions: &mut [Session]) {
        for session in sessions.iter_mut().filter(|s| s.task_unnumbered()) {
            match self.db.lock().unwrap().new_task_number() {
                Ok(number) => session.number_task(number),
                Err(err) => eprintln!("crystal daemon: couldn't number a task: {err:#}"),
            }
        }
    }

    /// Writes a task that has just closed in `session` into its project's
    /// history, as [`Daemon::write_down`] does. Then the distiller reads
    /// what it did.
    fn write_down_closed(&self, session: &Session, task: &TaskRecord) {
        self.write_down(session.cwd(), Some(&session.info()), task);
        self.distill_later(session, task);
    }

    /// Writes a task that has just closed, which ran in `cwd`, in `session`
    /// if it had started, into its project's history, tells of it, and
    /// keeps how it went in the project's memory. When it was done and was
    /// for a backlog item, ticks the item.
    fn write_down(&self, cwd: &Path, session: Option<&protocol::SessionInfo>, task: &TaskRecord) {
        let project = project::of(cwd).path;
        let ticked = {
            let mut db = self.db.lock().unwrap();
            if let Err(err) = db.record_task(&project, task) {
                eprintln!("crystal daemon: couldn't write down a closed task: {err:#}");
            }
            let done = task.state() == TaskState::Done;
            let ticks = done && backlog::enabled(&settings());
            match (ticks, task.backlog) {
                (true, Some(number)) => {
                    let ticked = db.change_backlog(&project, |store| {
                        store.mark(number, true, now_seconds())?;
                        Ok(store.get(number).cloned())
                    });
                    ticked.unwrap_or_else(|err| {
                        eprintln!(
                            "crystal daemon: couldn't tick #{number} on the backlog: {err:#}"
                        );
                        None
                    })
                }
                _ => None,
            }
        };
        let mut task = task.clone();
        if let Some(info) = session {
            self.hand_off(info, &task);
            task.artifacts = self.kept_with(task.id);
        }
        let closed = match session {
            Some(info) => Event::task(Kind::TaskClosed, info, task.clone()),
            None => Event::pending_task(Kind::TaskClosed, project, task.clone()),
        };
        self.events.emit(closed);
        self.tell_backlog(Kind::BacklogClosed, cwd, ticked);
        self.remember_outcome(cwd, &task);
    }

    /// Adds how `task` went, closed in the session `info` is about, to its
    /// worktree's handoff file, then keeps the file with the task: what the
    /// sessions after it there should know, and whoever reads the task
    /// later. A cancelled task leaves nothing to say.
    fn hand_off(&self, info: &SessionInfo, task: &TaskRecord) {
        let Some(outcome) = &task.outcome else {
            return;
        };
        let Some(worktree) = info.worktree.as_ref().map(|worktree| &worktree.path) else {
            return;
        };
        if outcome.cancelled || !handoff::enabled(&settings()) {
            return;
        }
        if let Some(note) = handoff::tidy(&outcome.summary) {
            let how = outcome.state().word();
            let heading =
                handoff::heading(&handoff::now(), &info.name, Some(&task.goal), Some(how));
            match self.add_to_handoff(worktree, &handoff::section(&heading, &note)) {
                Ok(path) => self.events.emit(Event::handoff(info, path, &note)),
                Err(err) => eprintln!("crystal daemon: couldn't add to a handoff file: {err:#}"),
            }
        }
        let Some(id) = task.id else {
            return;
        };
        let dir = state::task_dir(&self.socket, id);
        match artifacts::keep_handoff(&dir, &handoff::path(worktree)) {
            Ok(Some(kept)) => self.record_kept(info, task, &[kept]),
            Ok(None) => {}
            Err(err) => eprintln!("crystal daemon: couldn't keep t{id}'s handoff file: {err:#}"),
        }
    }

    /// Adds `section` to the handoff file of the worktree at `worktree`,
    /// one write at a time, kept out of git unless the config says its
    /// project keeps its notes there.
    fn add_to_handoff(&self, worktree: &Path, section: &str) -> Result<PathBuf> {
        let in_git = handoff::in_git(&settings(), &project::of(worktree).path);
        let _one_at_a_time = self.handoff.lock().unwrap();
        handoff::append(worktree, section, in_git)
    }

    /// Checks the files at `paths` and copies them into the directory of
    /// `session`'s task, before it closes: a file that can't be kept
    /// refuses the close, and keeps none of them.
    fn keep_files(&self, session: &mut Session, paths: &[PathBuf]) -> Result<Vec<Artifact>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        self.number_tasks(std::slice::from_mut(session));
        let id = session
            .task_id()
            .with_context(|| format!("{} has no task to keep files with", session.name))?;
        let checked = artifacts::check(paths, &session.checkout_top())?;
        let mut taken: Vec<String> = self
            .kept_with(Some(id))
            .into_iter()
            .map(|a| a.name)
            .collect();
        // The handoff file is kept under its own name as the task closes.
        taken.push(artifacts::HANDOFF_NAME.to_string());
        artifacts::keep(&state::task_dir(&self.socket, id), &checked, &taken)
    }

    /// Writes down the files `kept` with `task`, closed in the session
    /// `info` is about, and tells of each.
    fn record_kept(&self, info: &SessionInfo, task: &TaskRecord, kept: &[Artifact]) {
        let Some(id) = task.id else {
            return;
        };
        for artifact in kept {
            let added = self
                .db
                .lock()
                .unwrap()
                .add_artifact(id, artifact, now_seconds());
            match added {
                Ok(()) => self
                    .events
                    .emit(Event::artifact(info, task.clone(), artifact.clone())),
                Err(err) => {
                    eprintln!("crystal daemon: couldn't write down a file t{id} kept: {err:#}")
                }
            }
        }
    }

    /// The files kept with the task numbered `task`.
    fn kept_with(&self, task: Option<u64>) -> Vec<Artifact> {
        let Some(id) = task else {
            return Vec::new();
        };
        let kept = self.db.lock().unwrap().artifacts(id);
        kept.unwrap_or_else(|err| {
            eprintln!("crystal daemon: couldn't read the files t{id} kept: {err:#}");
            Vec::new()
        })
    }

    /// The paths of the files `crystal done --artifact` kept with the task
    /// numbered `task`: what a flow step's `{<step>.artifacts}` says.
    fn kept_files(&self, task: Option<u64>) -> Vec<PathBuf> {
        let kept = self.kept_with(task).into_iter();
        let files = kept.filter(|artifact| artifact.kind == ArtifactKind::File);
        files.map(|artifact| artifact.path).collect()
    }

    /// Has the distiller read what the task that just closed in `session`
    /// did, on a thread of its own, when memory is on and the config says
    /// to. A cancelled task did nothing anyone wanted kept.
    fn distill_later(&self, session: &Session, task: &TaskRecord) {
        let config = settings();
        let worth_reading = matches!(task.state(), TaskState::Done | TaskState::Failed);
        if !memory::enabled(&config) || !config.memory.distill || !worth_reading {
            return;
        }
        let Some(job) = self.distill_job(session, Some(task), config.memory) else {
            return;
        };
        let Some(reading) = Reading::start(&self.distilling, &session.id) else {
            return;
        };
        let events = self.events.clone();
        thread::spawn(move || {
            let name = &job.session;
            match distill::run(&job) {
                Ok(report) => {
                    eprintln!("crystal daemon: distilled {name}: {}", report.line());
                    for why in &report.rejected {
                        eprintln!("crystal daemon:   rejected {why}");
                    }
                    tell_distilled(&events, &job, &report.added);
                }
                Err(err) => eprintln!("crystal daemon: couldn't distill {name}: {err:#}"),
            }
            drop(reading);
        });
    }

    /// A pass of the distiller over what `session` did, on `task`: `None`
    /// when it did nothing the distiller can read.
    fn distill_job(
        &self,
        session: &Session,
        task: Option<&TaskRecord>,
        settings: MemorySettings,
    ) -> Option<Job> {
        let material = session.material()?;
        let header = match task {
            Some(task) => distill::header(task),
            None => format!(
                "The work of session {}, which wasn't started with a task.",
                session.name
            ),
        };
        Some(Job {
            socket: self.socket.clone(),
            project: memory::project_of(session.cwd()),
            checkout: session.checkout_top(),
            session: session.name.clone(),
            header,
            about: task.map(distill::about).unwrap_or_default(),
            material,
            env: session.env().clone(),
            settings,
        })
    }

    /// How the model that searches memory by meaning stands. Asked while
    /// the config says not to search with it, the daemon lets it go; while
    /// it has it loaded and entries have no vector yet, it gives them one,
    /// in the background, rather than at the next search.
    fn embedding_status(&self) -> Result<embed::Status> {
        let settings = settings().memory;
        embed::let_go_unless(&settings);
        let on_disk = embed::model_dir().map_or(0, |dir| embed::on_disk(&dir));
        let (entries, embedded) = memory::Store::open(&self.socket)?.counts(embed::MODEL)?;
        if embed::is_loaded() && embedded < entries {
            self.prepare_embeddings();
        }
        let preparing = self.preparing.lock().unwrap();
        Ok(embed::Status {
            on_disk,
            size: embed::size(),
            loaded: embed::is_loaded(),
            preparing: preparing.doing.map(String::from),
            failed: preparing.failed.clone(),
            entries,
            embedded,
        })
    }

    /// Gets the model that searches memory by meaning ready, on a thread of
    /// its own, unless that's being done already: downloads it if it isn't
    /// here, then, while the config still says to search with it, loads it
    /// and gives every entry its vector.
    fn prepare_embeddings(&self) {
        {
            let mut preparing = self.preparing.lock().unwrap();
            if preparing.doing.is_some() {
                return;
            }
            *preparing = Preparing {
                doing: Some("downloading the model"),
                failed: None,
            };
        }
        let preparing = self.preparing.clone();
        let socket = self.socket.clone();
        thread::spawn(move || {
            let doing = |what| preparing.lock().unwrap().doing = Some(what);
            let prepared = (|| -> Result<()> {
                let downloaded = embed::model_dir().is_some_and(|dir| embed::is_downloaded(&dir));
                if !downloaded {
                    embed::download(false)?;
                }
                doing("loading the model");
                let Some(embedder) = embed::shared_now() else {
                    return Ok(());
                };
                doing("embedding the entries");
                memory::Store::open(&socket)?.embed_missing(&*embedder)?;
                Ok(())
            })();
            let mut preparing = preparing.lock().unwrap();
            preparing.doing = None;
            if let Err(err) = prepared {
                eprintln!("crystal daemon: couldn't get the model ready: {err:#}");
                preparing.failed = Some(format!("{err:#}"));
            }
        });
    }

    /// Runs the distiller over what the session called `name` did, now,
    /// and says what came of it.
    fn distill_now(&self, name: &str) -> Result<Response> {
        let config = settings();
        crate::plugins::ensure_enabled(&config, "memory")?;
        let (job, reading) = {
            let mut sessions = self.sessions.lock().unwrap();
            let session = named(&mut sessions, name)?;
            let task = session.task_record();
            let job = self
                .distill_job(session, task.as_ref(), config.memory)
                .with_context(|| {
                    format!(
                        "{name} left nothing the distiller can read: \
                         only Claude Code's sessions and tasks do"
                    )
                })?;
            let reading = Reading::start(&self.distilling, &session.id)
                .with_context(|| format!("the distiller is reading what {name} did already"))?;
            (job, reading)
        };
        let report = distill::run(&job);
        drop(reading);
        let report = report?;
        tell_distilled(&self.events, &job, &report.added);
        Ok(Response::Distilled(report))
    }

    /// Keeps how a closed task turned out in its project's memory, for the
    /// sessions after it, when it was done or failed with something to say.
    fn remember_outcome(&self, cwd: &Path, task: &TaskRecord) {
        let Some(outcome) = &task.outcome else {
            return;
        };
        if outcome.summary.trim().is_empty() || outcome.cancelled {
            return;
        }
        let line = if outcome.failed {
            format!("{} (failed): {}", task.goal, outcome.summary)
        } else {
            format!("{}: {}", task.goal, outcome.summary)
        };
        let project = project::of(cwd).path;
        let kept =
            memory::record_outcome(&settings(), &self.socket, &project, &task.session, &line);
        match kept {
            Ok(Some(Added::New(entry))) => {
                self.events
                    .emit(Event::memory(Kind::MemoryAdded, project, entry));
            }
            Ok(_) => {}
            Err(err) => eprintln!("crystal daemon: couldn't remember how a task went: {err:#}"),
        }
    }

    /// The session called `name`, or the newest one, for a client about to
    /// show it. That counts as having seen it.
    fn find(&self, name: Option<&str>) -> Result<Found> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = match name {
            Some(name) => named(&mut sessions, name)?,
            None => sessions.last_mut().context("there are no sessions")?,
        };
        session.seen();
        self.tell_changes(session);
        Ok(Found {
            name: session.name.clone(),
            id: session.id.clone(),
            term: session.term(),
        })
    }

    fn is_task(&self, name: &str) -> Result<bool> {
        let mut sessions = self.sessions.lock().unwrap();
        Ok(named(&mut sessions, name)?.is_task())
    }

    /// The terminal of the session called `name`, to type into, which
    /// only makes sense while its program runs. The sessions are let go
    /// before any typing, which takes a moment. A session typed into by
    /// its name keeps that name: whoever types knows it by it.
    fn running_term(&self, name: &str) -> Result<Arc<Term>> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = named(&mut sessions, name)?;
        ensure!(session.is_running(), "{name} has ended");
        session.keep_name();
        Ok(session.term())
    }

    /// Names the session with id `id` from `prompt`, the first it was sent,
    /// when crystal named it after its program and the config says to.
    fn name_from_prompt(&self, sessions: &mut [Session], id: &str, prompt: &str) {
        let Some(index) = sessions.iter().position(|session| session.id == id) else {
            return;
        };
        if !sessions[index].is_named_after_program() || !settings().name_from_prompt {
            return;
        }
        // A prompt with nothing to name it by leaves it for the next.
        let Some(base) = names::from_prompt(prompt) else {
            return;
        };
        let taken = |name: &str| sessions.iter().any(|session| session.name == name);
        let new_name = unique_name(&base, taken);
        let session = &mut sessions[index];
        let old_name = std::mem::replace(&mut session.name, new_name);
        session.keep_name();
        self.tell_renamed(session, &old_name);
    }

    /// Tells that `session` was called `from` until now, and keeps a flow's
    /// step to it under its new name.
    fn tell_renamed(&self, session: &Session, from: &str) {
        self.events.emit(Event::renamed(&session.info(), from));
        for run in self.flows.lock().unwrap().iter_mut() {
            for step in &mut run.steps {
                if step.session.as_deref() == Some(from) {
                    step.session = Some(session.name.clone());
                }
            }
        }
    }

    fn handle(&self, request: Request) -> Result<Response> {
        match request {
            Request::Attach { .. } => bail!("attach takes over the connection"),
            Request::New(new) => self.new_session(new),
            Request::NewTask(task) => {
                let mut sessions = self.sessions.lock().unwrap();
                let name = start_task(
                    &mut sessions,
                    &self.socket,
                    &self.spending,
                    task,
                    None,
                    true,
                )?;
                Ok(self.started(&mut sessions, name, Kind::TaskOpened))
            }
            Request::List => {
                let sessions = self.sessions.lock().unwrap();
                Ok(Response::Sessions {
                    sessions: sessions.iter().map(Session::info).collect(),
                })
            }
            Request::Report {
                name,
                id,
                event,
                conversation,
                prompt,
            } => {
                let mut sessions = self.sessions.lock().unwrap();
                let id = match id {
                    Some(id) => id,
                    None => named(&mut sessions, &name)?.id.clone(),
                };
                if let Some(prompt) = prompt {
                    self.name_from_prompt(&mut sessions, &id, &prompt);
                }
                let session = with_id(&mut sessions, &id)?;
                if let Some(conversation) = conversation {
                    session.set_conversation(conversation);
                }
                // Reminded that its task is open, the agent carries on: its
                // turn hasn't ended, and it isn't done.
                if event == AgentEvent::TurnEnded
                    && tasks::enabled(&settings())
                    && session.remind_of_task()
                {
                    return Ok(Response::Remind {
                        text: tasks::REMINDER.to_string(),
                    });
                }
                // An agent that reports for itself holds the session's
                // status: what hooks say counts again once it lets go.
                if !session.is_claimed() {
                    session.on_agent_event(event);
                }
                self.tell_changes(session);
                Ok(Response::Done)
            }
            Request::ReportAgent { id, name, report } => {
                let mut sessions = self.sessions.lock().unwrap();
                let session = match (id, name) {
                    (Some(id), _) => with_id(&mut sessions, &id)?,
                    (None, Some(name)) => named(&mut sessions, &name)?,
                    (None, None) => bail!("say which session the report is about"),
                };
                ensure!(
                    !session.is_task(),
                    "{} is a background task, which says what it's doing itself",
                    session.name
                );
                ensure!(session.is_running(), "{} has ended", session.name);
                session.take_report(report)?;
                self.tell_changes(session);
                Ok(Response::Done)
            }
            Request::Kill { name } => {
                let mut sessions = self.sessions.lock().unwrap();
                let index = sessions
                    .iter()
                    .position(|session| session.name == name)
                    .with_context(|| format!("no session named {name}"))?;
                let mut session = sessions.remove(index);
                let cancelled = tasks::enabled(&settings())
                    .then(|| session.cancel_task("its session was killed"))
                    .flatten();
                if let Some(cancelled) = cancelled {
                    self.write_down_closed(&session, &cancelled);
                }
                let info = session.info();
                session.stop();
                if info.state == State::Running {
                    self.events.emit(Event::ended(&info, "killed".into()));
                }
                self.events
                    .emit(Event::about_session(Kind::SessionRemoved, &info));
                Ok(Response::Done)
            }
            Request::Emit { event } => {
                self.events.emit(*event);
                Ok(Response::Done)
            }
            Request::Subscribe { .. } | Request::WaitOutput { .. } => {
                bail!("this takes the connection over")
            }
            Request::Rename { name, new_name } => {
                let mut sessions = self.sessions.lock().unwrap();
                if new_name != name {
                    check_name(&new_name, |taken| {
                        sessions.iter().any(|session| session.name == taken)
                    })?;
                }
                let session = named(&mut sessions, &name)?;
                session.name = new_name.clone();
                session.keep_name();
                if new_name != name {
                    self.tell_renamed(session, &name);
                }
                Ok(Response::Done)
            }
            Request::Respawn { name, env } => self.respawn(&name, env),
            Request::Send { name, text, enter } => {
                // In a block of its own, so the sessions are let go before
                // the typing below, which takes a moment.
                {
                    let mut sessions = self.sessions.lock().unwrap();
                    let session = named(&mut sessions, &name)?;
                    // A task takes text as a follow-up: another run that
                    // carries its conversation on.
                    if session.is_task() {
                        session.prompt(&text)?;
                        return Ok(Response::Done);
                    }
                }
                let term = self.running_term(&name)?;
                term.write(&typing::keystrokes(&text, term.wants_bracketed_paste()))?;
                if enter {
                    thread::sleep(typing::ENTER_PAUSE);
                    term.write(typing::ENTER)?;
                }
                Ok(Response::Done)
            }
            Request::SendKeys { name, keys } => {
                ensure!(
                    !self.is_task(&name)?,
                    "{name} is a task, which takes no keys: \
                     `crystal send {name} \"…\"` gives it a follow-up"
                );
                let term = self.running_term(&name)?;
                for key in &keys {
                    term.write(&term.keystrokes(key))?;
                }
                Ok(Response::Done)
            }
            Request::Result { name } => {
                let mut sessions = self.sessions.lock().unwrap();
                let result = named(&mut sessions, &name)?.result()?;
                Ok(Response::Result(result))
            }
            Request::Read { name, history } => {
                let mut sessions = self.sessions.lock().unwrap();
                let rows = named(&mut sessions, &name)?.term().rows(history);
                Ok(Response::Screen { rows })
            }
            Request::Close {
                id,
                name,
                failed,
                summary,
                artifacts,
            } => {
                tasks::ensure_enabled(&settings())?;
                let mut sessions = self.sessions.lock().unwrap();
                let session = id_or_name(&mut sessions, id, name)?;
                let state = if failed {
                    TaskState::Failed
                } else {
                    TaskState::Done
                };
                let kept = self.keep_files(session, &artifacts)?;
                let closed = session.close_task(state, &summary)?;
                self.record_kept(&session.info(), &closed, &kept);
                self.write_down_closed(session, &closed);
                Ok(Response::Done)
            }
            Request::Handoff { id, name, note } => {
                handoff::ensure_enabled(&settings())?;
                let note = handoff::tidy(&note).context(
                    "the note is empty: `crystal handoff \"<what the next session here should know>\"`",
                )?;
                let (info, task) = {
                    let mut sessions = self.sessions.lock().unwrap();
                    let session = id_or_name(&mut sessions, id, name)?;
                    let open = session.task_record().filter(|task| task.outcome.is_none());
                    (session.info(), open.map(|task| task.goal))
                };
                let worktree = info.worktree.as_ref().map(|worktree| &worktree.path);
                let worktree = worktree.with_context(|| {
                    format!(
                        "{} isn't in a git worktree, where notes are kept",
                        info.name
                    )
                })?;
                let heading = handoff::heading(&handoff::now(), &info.name, task.as_deref(), None);
                let path = self.add_to_handoff(worktree, &handoff::section(&heading, &note))?;
                self.events.emit(Event::handoff(&info, path, &note));
                Ok(Response::Done)
            }
            Request::Distill { name } => self.distill_now(&name),
            Request::EmbeddingStatus => Ok(Response::EmbeddingStatus(self.embedding_status()?)),
            Request::PrepareEmbeddings => {
                self.prepare_embeddings();
                Ok(Response::Done)
            }
            Request::SearchMemory {
                dir,
                query,
                kind,
                limit,
            } => {
                crate::plugins::ensure_enabled(&settings(), "memory")?;
                let project = memory::project_of(&dir);
                let embedder = embed::shared_now();
                let mut store = memory::Store::open(&self.socket)?;
                let found =
                    store.search(&project, &query, kind, limit, embed::as_embed(&embedder))?;
                Ok(Response::Memory {
                    entries: memory::freshest_first(found, &project),
                })
            }
            Request::Tasks { dir, all } => {
                tasks::ensure_enabled(&settings())?;
                Ok(Response::Tasks {
                    tasks: self.tasks(&dir, all),
                })
            }
            Request::AddTask(mut task) => {
                tasks::ensure_enabled(&settings())?;
                task.created = now_seconds();
                let id = self.db.lock().unwrap().add_pending_task(&task)?;
                let project = project::of(&task.cwd).path;
                let record = tasks::pending_record(&PendingTask { id, ..task });
                self.events
                    .emit(Event::pending_task(Kind::TaskOpened, project, record));
                Ok(Response::TaskAdded { id })
            }
            Request::StartTask { id, env } => self.start_pending(id, env),
            Request::ShowTask { task } => {
                tasks::ensure_enabled(&settings())?;
                Ok(Response::Task(self.with_kept(self.find_task(&task)?)))
            }
            Request::CancelTask { task } => {
                self.cancel_task(&task)?;
                Ok(Response::Done)
            }
            Request::TaskLog { task } => {
                tasks::ensure_enabled(&settings())?;
                let task = self.with_kept(self.find_task(&task)?);
                let transcript = self.transcript_of(&task);
                Ok(Response::TaskLog { task, transcript })
            }
            Request::Answer {
                task,
                answer,
                message,
            } => {
                let mut sessions = self.sessions.lock().unwrap();
                let session = task_session(&mut sessions, &task)?;
                let asked = session.info().asking;
                session.answer(answer, message.as_deref())?;
                let info = session.info();
                self.events.emit(Event::answered(&info, asked, answer));
                // The next it asks for, if it asked for several at once.
                if let Some(next) = info.asking.clone() {
                    self.events.emit(Event::asking(&info, next));
                }
                Ok(Response::Done)
            }
            Request::Interrupt { task } => {
                let mut sessions = self.sessions.lock().unwrap();
                let session = task_session(&mut sessions, &task)?;
                session.interrupt()?;
                self.events
                    .emit(Event::about_session(Kind::RunInterrupted, &session.info()));
                Ok(Response::Done)
            }
            Request::Spending => Ok(Response::Spending(protocol::Spending {
                today_usd: self.spending.today(),
                daily_budget_usd: settings().tasks.daily_budget_usd,
            })),
            Request::BacklogList { dir, all } => {
                backlog::ensure_enabled(&settings())?;
                let project = project::of(&dir);
                let store = self.db.lock().unwrap().backlog(&project.path)?;
                Ok(Response::Backlog(Backlog {
                    project: project.name,
                    path: project.path,
                    items: store.items(all),
                }))
            }
            Request::BacklogAdd { dir, text, tags } => {
                let (number, item) = self.change_backlog(&dir, |store| {
                    let number = store.add(&text, tags, now_seconds())?;
                    Ok((number, store.get(number).cloned()))
                })?;
                self.tell_backlog(Kind::BacklogAdded, &dir, item);
                Ok(Response::Added { number })
            }
            Request::BacklogMark { dir, number, done } => {
                let item = self.change_backlog(&dir, |store| {
                    store.mark(number, done, now_seconds())?;
                    Ok(store.get(number).cloned())
                })?;
                if done {
                    self.tell_backlog(Kind::BacklogClosed, &dir, item);
                }
                Ok(Response::Done)
            }
            Request::BacklogRemove { dir, number } => {
                self.change_backlog(&dir, |store| store.remove(number))?;
                Ok(Response::Done)
            }
            Request::BacklogCounts { projects } => {
                backlog::ensure_enabled(&settings())?;
                let mut db = self.db.lock().unwrap();
                let open = projects
                    .into_iter()
                    .map(|path| {
                        let count = db.backlog(&path)?.open_count();
                        Ok((path, count))
                    })
                    .collect::<Result<_>>()?;
                Ok(Response::BacklogCounts { open })
            }
            Request::StartFlow {
                flow,
                goal,
                cwd,
                env,
            } => {
                let run = self.start_flow(&flow, goal, cwd, env)?;
                Ok(Response::FlowStarted { run })
            }
            Request::ListFlows => {
                flows::ensure_enabled(&settings())?;
                let runs = self.flows.lock().unwrap().clone();
                Ok(Response::Flows { runs })
            }
            Request::ApproveFlow { run } => {
                self.change_flow(&run, None, FlowRun::approve)?;
                Ok(Response::Done)
            }
            Request::SendFlowBack { run, notes } => {
                self.change_flow(&run, Some(&notes), |run| run.send_back(&notes))?;
                Ok(Response::Done)
            }
            Request::RetryFlow { run } => {
                self.change_flow(&run, None, FlowRun::retry)?;
                Ok(Response::Done)
            }
            Request::CancelFlow { run } => {
                self.cancel_flow(&run)?;
                Ok(Response::Done)
            }
            // Leaving is enough: the sessions' terminals close with the
            // daemon, and the list of them stays as it was last written.
            Request::Shutdown {
                keep_sessions: true,
            } => Ok(Response::Done),
            Request::Shutdown {
                keep_sessions: false,
            } => {
                let mut sessions = std::mem::take(&mut *self.sessions.lock().unwrap());
                let tasks_on = tasks::enabled(&settings());
                for session in &mut sessions {
                    let cancelled = tasks_on
                        .then(|| session.cancel_task("crystal was stopped"))
                        .flatten();
                    if let Some(cancelled) = cancelled {
                        self.write_down(session.cwd(), Some(&session.info()), &cancelled);
                    }
                    session.stop();
                }
                let deadline = Instant::now() + STOP_GRACE + Duration::from_millis(500);
                while sessions.iter().any(Session::is_running) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(20));
                }
                // Asked to stop, the sessions stay stopped.
                if let Err(err) = self.db.lock().unwrap().save_sessions(&[]) {
                    eprintln!("crystal daemon: couldn't forget the sessions: {err:#}");
                }
                Ok(Response::Done)
            }
        }
    }

    /// Changes the backlog of the project `dir` is in with `change`, and
    /// writes it back when that worked.
    fn change_backlog<T>(
        &self,
        dir: &Path,
        change: impl FnOnce(&mut backlog::Store) -> Result<T>,
    ) -> Result<T> {
        backlog::ensure_enabled(&settings())?;
        let project = project::of(dir);
        self.db
            .lock()
            .unwrap()
            .change_backlog(&project.path, change)
    }

    /// The tasks of the project `dir` is in, or every project's with `all`:
    /// those still open in sessions, then those waiting to start, then
    /// those closed, the latest first. A task closed more than once, say a
    /// background task given a follow-up, is listed as it last closed.
    fn tasks(&self, dir: &Path, all: bool) -> Vec<TaskView> {
        let project = project::of(dir);
        let in_project = |cwd: &Path| all || project::of(cwd).path == project.path;
        let mut tasks: Vec<TaskView> = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .iter()
                .filter(|session| in_project(session.cwd()))
                .filter_map(Session::task_view)
                .filter(|task| task.record.outcome.is_none())
                .collect()
        };
        let pending = self.db.lock().unwrap().pending_tasks();
        let pending = pending.unwrap_or_else(|err| {
            eprintln!("crystal daemon: couldn't read the tasks waiting to start: {err:#}");
            Vec::new()
        });
        let pending = pending.iter().filter(|task| in_project(&task.cwd));
        tasks.extend(pending.map(|task| TaskView::of_record(tasks::pending_record(task))));
        let mine = (!all).then_some(project.path.as_path());
        let mut closed = self.closed_tasks(mine);
        let mut listed: HashSet<u64> = tasks.iter().filter_map(|task| task.record.id).collect();
        closed.retain(|task| task.id.is_none_or(|id| listed.insert(id)));
        tasks.extend(closed.into_iter().map(TaskView::of_record));
        tasks.into_iter().map(|task| self.with_kept(task)).collect()
    }

    /// `task` with the files kept with it.
    fn with_kept(&self, mut task: TaskView) -> TaskView {
        task.record.artifacts = self.kept_with(task.record.id);
        task
    }

    /// The closed tasks of the project whose main worktree is `project`, or
    /// of every project, the latest to close first.
    fn closed_tasks(&self, project: Option<&Path>) -> Vec<TaskRecord> {
        let closed = self.db.lock().unwrap().closed_tasks(project);
        let mut closed = closed.unwrap_or_else(|err| {
            eprintln!("crystal daemon: couldn't read the closed tasks: {err:#}");
            Vec::new()
        });
        closed.sort_by_key(|task| {
            std::cmp::Reverse(task.outcome.as_ref().map_or(0, |outcome| outcome.closed))
        });
        closed
    }

    /// The task `handle` names, by its number or its session's name: open
    /// in a session, waiting to start, or closed.
    fn find_task(&self, handle: &str) -> Result<TaskView> {
        {
            let mut sessions = self.sessions.lock().unwrap();
            if let Some(session) = handled_session(&mut sessions, handle) {
                return session
                    .task_view()
                    .with_context(|| format!("{} has no task", session.name));
            }
        }
        let id = tasks::parse_id(handle).with_context(|| {
            format!("there's no task or session called {handle}: give a task's number, like t12")
        })?;
        if let Some(task) = self.db.lock().unwrap().pending_task(id)? {
            return Ok(TaskView::of_record(tasks::pending_record(&task)));
        }
        let closed = self.closed_tasks(None);
        let found = closed.into_iter().find(|task| task.id == Some(id));
        let found = found.with_context(|| format!("there's no task t{id}"))?;
        Ok(TaskView::of_record(found))
    }

    /// The transcript of the session working on `task`, or that worked on
    /// it, while that session is still there: its history, then its screen.
    fn transcript_of(&self, task: &TaskView) -> Option<Vec<String>> {
        let sessions = self.sessions.lock().unwrap();
        let session = match task.record.id {
            Some(id) => sessions
                .iter()
                .find(|session| session.task_id() == Some(id)),
            None => sessions
                .iter()
                .find(|session| session.name == task.record.session),
        }?;
        Some(session.term().rows(true))
    }

    /// Cancels the task `handle` names, by its number or its session's
    /// name, and stops its session; or, waiting to start, takes it out of
    /// the store.
    fn cancel_task(&self, handle: &str) -> Result<()> {
        tasks::ensure_enabled(&settings())?;
        {
            let mut sessions = self.sessions.lock().unwrap();
            if let Some(session) = handled_session(&mut sessions, handle) {
                let name = session.name.clone();
                let cancelled = session
                    .cancel_task("cancelled by the user")
                    .with_context(|| format!("{name} has no task that's open"))?;
                self.write_down_closed(session, &cancelled);
                session.stop();
                return Ok(());
            }
        }
        let id = tasks::parse_id(handle).with_context(|| {
            format!("there's no task or session called {handle}: give a task's number, like t12")
        })?;
        let pending = {
            let db = self.db.lock().unwrap();
            let pending = db.pending_task(id)?;
            let pending = pending.with_context(|| format!("there's no open task t{id}"))?;
            db.remove_pending_task(id)?;
            pending
        };
        let closed = now_seconds();
        let cancelled = TaskRecord {
            outcome: Some(TaskOutcome::new(
                TaskState::Cancelled,
                "cancelled before it started",
                closed,
            )),
            pending: false,
            ..tasks::pending_record(&pending)
        };
        self.write_down(&pending.cwd, None, &cancelled);
        Ok(())
    }

    /// Starts the task numbered `id`, which was waiting to, from the
    /// client's environment `env`. One that won't start waits on.
    fn start_pending(&self, id: u64, env: BTreeMap<String, String>) -> Result<Response> {
        tasks::ensure_enabled(&settings())?;
        // Held to the end, so that the task can't be started twice at once.
        let mut sessions = self.sessions.lock().unwrap();
        let task = self.db.lock().unwrap().pending_task(id)?;
        let task = task.with_context(|| format!("there's no task t{id} waiting to start"))?;
        let PendingTask {
            goal,
            cwd,
            name,
            start: how,
            backlog,
            ..
        } = task;
        let started = match how {
            TaskStart::Agent { command } => {
                let new = NewSession {
                    name,
                    cwd,
                    command,
                    env,
                    task: Some(goal),
                    backlog,
                };
                start(&mut sessions, &self.socket, new, None, None)
            }
            TaskStart::Background { args } => {
                let new = NewTask {
                    name,
                    cwd,
                    spec: TaskSpec { prompt: goal, args },
                    env,
                    backlog,
                };
                start_task(&mut sessions, &self.socket, &self.spending, new, None, true)
            }
        };
        let name = started?;
        // It keeps the number it was given as it was made.
        if let Some(session) = sessions.last_mut() {
            session.number_task(id);
        }
        if let Err(err) = self.db.lock().unwrap().remove_pending_task(id) {
            eprintln!("crystal daemon: couldn't forget that t{id} waits to start: {err:#}");
        }
        Ok(self.started(&mut sessions, name, Kind::TaskStarted))
    }

    fn new_session(&self, new: NewSession) -> Result<Response> {
        let mut sessions = self.sessions.lock().unwrap();
        let name = start(&mut sessions, &self.socket, new, None, None)?;
        Ok(self.started(&mut sessions, name, Kind::TaskOpened))
    }

    /// What's said once the session called `name` has started: its task is
    /// numbered, it's told of, its task as `task_kind`, and the client is
    /// told both.
    fn started(&self, sessions: &mut [Session], name: String, task_kind: Kind) -> Response {
        self.number_tasks(sessions);
        self.tell_started(sessions, &name, task_kind);
        let session = sessions.iter().find(|session| session.name == name);
        let task = session.and_then(Session::task_id);
        Response::Created { name, task }
    }

    /// Tells that the session called `name` has started, and of its task,
    /// when it has one open: opened with it, or `task_kind`, started after
    /// it waited to.
    fn tell_started(&self, sessions: &[Session], name: &str, task_kind: Kind) {
        let Some(session) = sessions.iter().find(|session| session.name == name) else {
            return;
        };
        let info = session.info();
        self.events
            .emit(Event::about_session(Kind::SessionStarted, &info));
        if let Some(task) = session.task_record().filter(|task| task.outcome.is_none()) {
            self.events.emit(Event::task(task_kind, &info, task));
        }
    }

    /// Tells what has happened to `session` since this was last asked.
    fn tell_changes(&self, session: &mut Session) {
        let changes = session.take_changes();
        if changes.is_empty() {
            return;
        }
        let info = session.info();
        let task = |kind| {
            let task = session.task_record()?;
            Some(Event::task(kind, &info, task))
        };
        for change in changes {
            let event = match change {
                Change::Activity { from, to: Some(to) } => Some(Event::activity(&info, from, to)),
                // The agent has left the front, and there's nothing to
                // say it's doing.
                Change::Activity { to: None, .. } => None,
                Change::RunStarted { prompt } => Some(Event::run_started(&info, &prompt)),
                Change::RunEnded(result) => Some(Event::run_ended(&info, &result)),
                Change::Asking(asking) => Some(Event::asking(&info, asking)),
                Change::Reopened => task(Kind::TaskOpened),
                Change::TaskWaiting => task(Kind::TaskWaiting),
                Change::Claimed => Some(Event::about_session(Kind::SessionClaimed, &info)),
                Change::Released { agent } => Some(Event::released(&info, &agent)),
            };
            if let Some(event) = event {
                self.events.emit(event);
            }
        }
    }

    /// Tells of `item`, on the backlog of the project `dir` is in.
    fn tell_backlog(&self, kind: Kind, dir: &Path, item: Option<protocol::BacklogItem>) {
        if let Some(item) = item {
            let project = project::of(dir).path;
            self.events.emit(Event::backlog(kind, project, item));
        }
    }

    /// Streams the events `filter` takes to a client, after those the log
    /// has from `since`, until it hangs up or falls too far behind.
    fn stream_events(&self, conn: &UnixStream, filter: Filter, since: Option<Since>) -> Result<()> {
        let subscription = self.events.subscribe(filter.clone());
        let streamed = stream(conn, &self.events, &filter, since, &subscription);
        self.events.unsubscribe(subscription.id);
        let _ = conn.shutdown(Shutdown::Both);
        streamed
    }

    /// Answers a client waiting for a row on the screen of the session
    /// called `name` to match `pattern`, once one does, the program ends,
    /// or `timeout` passes; or lets it go when it hangs up.
    fn wait_for_output(
        &self,
        conn: &UnixStream,
        name: &str,
        pattern: &str,
        timeout: Option<Duration>,
    ) -> Result<()> {
        let watched = (|| {
            let pattern = Regex::new(pattern)
                .with_context(|| format!("`{pattern}` isn't a regular expression"))?;
            let term = {
                let mut sessions = self.sessions.lock().unwrap();
                named(&mut sessions, name)?.term()
            };
            let hung_up = watch_for_hang_up(conn)?;
            matching_row(&term, name, &pattern, timeout, &hung_up)
        })();
        let response = match watched {
            Ok(None) => None,
            Ok(Some(line)) => Some(Response::Matched { line }),
            Err(err) => Some(Response::from(err)),
        };
        if let Some(response) = response {
            protocol::send(conn, &response)?;
        }
        let _ = conn.shutdown(Shutdown::Both);
        Ok(())
    }

    /// Runs an ended session's command again, in its directory and under
    /// its name, keeping its place in the list. An agent whose conversation
    /// can be picked up starts back in it.
    fn respawn(&self, name: &str, env: BTreeMap<String, String>) -> Result<Response> {
        let mut sessions = self.sessions.lock().unwrap();
        let index = sessions
            .iter()
            .position(|session| session.name == name)
            .with_context(|| format!("no session named {name}"))?;
        ensure!(!sessions[index].is_running(), "{name} is still running");

        // The ended session makes way for the new one, and comes back if
        // that doesn't start.
        let ended = sessions.remove(index);
        let launch = ended.launch();
        // Run again, its task is open again: the work goes on.
        let backlog = launch.goal.as_ref().and_then(|goal| goal.backlog);
        let started = match launch.task {
            // A task runs its prompt again, in its conversation if it had
            // got as far as one.
            Some(spec) => {
                let task = NewTask {
                    name: Some(launch.name),
                    cwd: launch.cwd,
                    spec,
                    env,
                    backlog,
                };
                let conversation = launch.conversation.map(|conversation| conversation.id);
                start_task(
                    &mut sessions,
                    &self.socket,
                    &self.spending,
                    task,
                    conversation,
                    true,
                )
            }
            None => {
                let new = NewSession {
                    name: Some(launch.name),
                    cwd: launch.cwd,
                    command: launch.command,
                    env,
                    task: launch.goal.as_ref().map(|goal| goal.goal.clone()),
                    backlog,
                };
                start(
                    &mut sessions,
                    &self.socket,
                    new,
                    launch.conversation,
                    launch.resume,
                )
            }
        };
        if let Err(err) = started {
            sessions.insert(index, ended);
            return Err(err);
        }
        // `start` adds the new session at the end; it goes where the old
        // one was.
        let mut started = sessions.pop().expect("start added a session");
        // It's the same task, open again, under the same number.
        if let (Some(goal), true) = (launch.goal, started.task_record().is_some()) {
            started.give_task(TaskInfo {
                waiting: false,
                outcome: None,
                ..goal
            });
        }
        sessions.insert(index, started);
        self.tell_started(&sessions, name, Kind::TaskOpened);
        Ok(Response::Done)
    }
}

/// The session `step` of `run` runs in, while it's there.
fn step_session<'a>(
    sessions: &'a mut [Session],
    run: &FlowRun,
    step: usize,
) -> Option<&'a mut Session> {
    let name = run.steps[step].session.as_ref()?;
    sessions.iter_mut().find(|session| session.name == *name)
}

/// Sends a subscriber what it asked for: that it has started, the events
/// from the log it wants to catch up on, then each new one as it comes.
/// A client that has gone ends it, and so does falling too far behind,
/// which it's told.
fn stream(
    conn: &UnixStream,
    events: &Bus,
    filter: &Filter,
    since: Option<Since>,
    subscription: &Subscription,
) -> Result<()> {
    protocol::send(
        conn,
        &Response::Subscribed {
            seq: subscription.seq,
        },
    )?;
    if let Some(since) = since {
        for event in events.replay(filter, since, subscription.seq) {
            if protocol::send(conn, &event).is_err() {
                return Ok(());
            }
        }
    }
    let hung_up = watch_for_hang_up(conn)?;
    loop {
        match subscription.feed.recv_timeout(LOOK_FOR_HANG_UP) {
            Ok(event) => {
                if protocol::send(conn, &*event).is_err() {
                    return Ok(());
                }
            }
            Err(RecvTimeoutError::Timeout) if hung_up.load(Ordering::Relaxed) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                let message = "the stream fell too far behind and lost events: \
                               subscribe again, from the last one you got"
                    .to_string();
                return Ok(protocol::send(conn, &Response::Error { message })?);
            }
        }
    }
}

/// Waits, looking again each time the program writes, until a row of
/// `term`'s screen, or of the end of its history, matches `pattern`, and
/// gives that row; or says why none did: the program ended, or `timeout`
/// passed. `None` once the client has hung up.
fn matching_row(
    term: &Term,
    name: &str,
    pattern: &Regex,
    timeout: Option<Duration>,
    hung_up: &AtomicBool,
) -> Result<Option<String>> {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let output = term.listen();
    let mut ended = false;
    loop {
        let looked = Instant::now();
        let rows = term.recent_rows(HISTORY_MATCHED);
        if let Some(row) = rows.iter().find(|row| pattern.is_match(row)) {
            return Ok(Some(row.trim_end().to_string()));
        }
        if ended {
            bail!("{name} has ended, and nothing on its screen matches `{pattern}`");
        }
        let left = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        if left == Some(Duration::ZERO) {
            let seconds = timeout.unwrap_or_default().as_secs_f64();
            bail!("nothing on {name}'s screen matched `{pattern}` after {seconds}s");
        }
        let wait = left.map_or(LOOK_FOR_HANG_UP, |left| left.min(LOOK_FOR_HANG_UP));
        match output.recv_timeout(wait) {
            Ok(()) => thread::sleep(LOOK_AT_MOST_EVERY.saturating_sub(looked.elapsed())),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => ended = true,
        }
        if hung_up.load(Ordering::Relaxed) {
            return Ok(None);
        }
    }
}

/// Watches, on a thread of its own, for a client that only listens to hang
/// up: it sends nothing after its request, so a read that returns at all
/// means it has gone. Shutting the connection down ends the watch too.
fn watch_for_hang_up(conn: &UnixStream) -> Result<Arc<AtomicBool>> {
    let hung_up = Arc::new(AtomicBool::new(false));
    let mut watched = conn.try_clone()?;
    thread::spawn({
        let hung_up = hung_up.clone();
        move || {
            let _ = watched.read(&mut [0]);
            hung_up.store(true, Ordering::Relaxed);
        }
    });
    Ok(hung_up)
}

/// Tells of the entries the distiller added to the memory of `job`'s
/// project, by their ids.
fn tell_distilled(events: &Bus, job: &Job, added: &[u64]) {
    let Ok(mut store) = memory::Store::open(&job.socket) else {
        return;
    };
    for &id in added {
        if let Ok(Some(entry)) = store.get(&job.project, id) {
            events.emit(Event::memory(Kind::MemoryAdded, job.project.clone(), entry));
        }
    }
}

/// How the run of `step` ended, once it has: from what its task's run came
/// to, or a failure when its session has gone.
fn how_step_ended(sessions: &[Session], run: &FlowRun, step: usize) -> Option<Ended> {
    let name = run.steps[step].session.as_ref()?;
    let Some(session) = sessions.iter().find(|session| session.name == *name) else {
        return Some(Ended {
            failed: true,
            answer: format!("its session, {name}, has gone"),
            cost_usd: 0.0,
            artifacts: Vec::new(),
        });
    };
    if !session.is_task() {
        // A step in a terminal ends as its task closes.
        let outcome = session.task_record()?.outcome?;
        return Some(Ended {
            failed: outcome.state() != TaskState::Done,
            answer: outcome.summary,
            cost_usd: 0.0,
            artifacts: Vec::new(),
        });
    }
    let result = session.finished_run()?;
    Some(Ended {
        failed: result.failed || !session.is_running(),
        answer: result.text,
        cost_usd: result.cost_usd,
        artifacts: Vec::new(),
    })
}

/// A session found for a client about to show it.
struct Found {
    name: String,
    id: String,
    term: Arc<Term>,
}

/// Starts a session and adds it to `sessions`. Given a `conversation`, an
/// agent that can pick one up starts back in it; given a `resume_command`,
/// the command an agent said resumes it, it's resumed with that instead.
fn start(
    sessions: &mut Vec<Session>,
    socket: &Path,
    new: NewSession,
    conversation: Option<Conversation>,
    resume_command: Option<Vec<String>>,
) -> Result<String> {
    let NewSession {
        name,
        cwd,
        command,
        env,
        task,
        backlog,
    } = new;
    let Some(program) = command.first() else {
        bail!("no command to run");
    };
    ensure!(
        exists(program, &cwd, env.get("PATH")),
        "command not found: {program}"
    );
    let config = settings();
    // Not given a name, a session is named for what it's asked to do, or
    // else after its program, until its first prompt names it.
    let from_prompt = task
        .as_deref()
        .filter(|_| config.name_from_prompt)
        .and_then(names::from_prompt);
    let named_after_program = name.is_none() && from_prompt.is_none();
    let taken = |name: &str| sessions.iter().any(|session| session.name == name);
    let name = match name {
        Some(name) => {
            check_name(&name, taken)?;
            name
        }
        None => unique_name(from_prompt.as_deref().unwrap_or(program), taken),
    };

    let id = new_id();
    let rollouts = codex::Rollouts::for_session(&command, &cwd, &env);
    // An agent that said how to resume it comes back with that command:
    // typed into the session's shell, or else run in place of its command.
    // It says what it's doing again once it's up.
    let resume_command = resume_command.filter(|argv| resumable(argv, &name, &config, &cwd, &env));
    let at_a_shell = matches!(front::of_command(&command), Some(Front::Shell { .. }));
    let resumed = resume_command.is_some();
    let (asked, typed) = match resume_command {
        Some(argv) if at_a_shell => (command.clone(), Some(report::typed(&argv))),
        Some(argv) => (argv, None),
        None => (command.clone(), None),
    };
    let env = env::for_session(&env, &name, &id, socket);
    let crystal = std::env::current_exe()?;
    // A conversation that can't be picked up any more is left behind: the
    // agent starts a new one, which its hooks or its rollout will name.
    let conversation = conversation
        .filter(|_| !resumed)
        .filter(Conversation::can_resume);
    let resume = conversation
        .as_ref()
        .map(|conversation| conversation.id.as_str());
    // What it was started to do: a conversation picked up again has been
    // asked that already, whether tasks are on or off.
    let given_task = task.clone();
    // With tasks off, a session started with something to do is just a
    // session.
    let task = task.filter(|_| tasks::enabled(&config));
    let about_task = task
        .as_ref()
        .map(|_| tasks::instructions(backlog::enabled(&config)));
    let remembered = remembered(socket, &cwd, &command);
    let said = [
        task.as_deref(),
        about_task.as_deref(),
        remembered.as_deref(),
    ];
    let handoff = handoff_note(&cwd, &said);
    // Picked up again with its own command, its conversation has heard
    // crystal's notes already.
    let instructions = if resumed {
        Vec::new()
    } else {
        notes(about_task, handoff, remembered)
    };
    let argv = agents::argv(
        &asked,
        &crystal,
        resume,
        given_task.as_deref(),
        &instructions,
    );
    let argv = agents::with_options(argv, &memory_tools(socket, &cwd, &asked, &crystal));
    let argv = codex::with_instructions(argv, &instructions, codex::home(&env).as_deref());
    let mut session = Session::spawn(id, name.clone(), command, &argv, cwd, &env)?;
    if let Some(typed) = typed
        && let Err(err) = session.term().write(&typed)
    {
        eprintln!("crystal daemon: couldn't resume {name}'s agent: {err:#}");
    }
    if named_after_program {
        session.mark_named_after_program();
    }
    if let Some(goal) = task {
        session.give_task(new_task_info(goal, false, backlog));
    }
    match conversation {
        Some(conversation) => session.set_conversation(conversation),
        None => {
            if let Some(rollouts) = rollouts {
                session.look_for_conversation_in(rollouts);
            }
        }
    }
    sessions.push(session);
    Ok(name)
}

/// Whether the session called `name` can be resumed with `argv`, the
/// command its agent gave, run from `cwd` in the environment `env`: the
/// config says to, and the command is there.
fn resumable(
    argv: &[String],
    name: &str,
    config: &Config,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> bool {
    let Some(program) = argv.first().filter(|_| config.resume_reported_agents) else {
        return false;
    };
    let found = exists(program, cwd, env.get("PATH"));
    if !found {
        eprintln!("crystal daemon: couldn't resume {name}'s agent: command not found: {program}");
    }
    found
}

/// What crystal tells an agent on top of what it was asked, a paragraph
/// each: about its task first, the one thing it mustn't forget, then the
/// notes the sessions before it in its worktree left, then what the
/// project's memory has, all opened by where they come from. Nothing at
/// all when there's nothing to say.
fn notes(
    about_task: Option<String>,
    handoff: Option<String>,
    remembered: Option<String>,
) -> Vec<String> {
    let said: Vec<String> = about_task
        .into_iter()
        .chain(handoff)
        .chain(remembered)
        .collect();
    if said.is_empty() {
        return said;
    }
    let mut notes = vec![agents::ABOUT_CRYSTAL.to_string()];
    notes.extend(said);
    notes
}

/// What an agent starting in `cwd` is told of its worktree's handoff file,
/// when the plugin is on and the file has notes. `said` is what else it's
/// asked and told as it starts, which the file's end mustn't push past what
/// a prompt may be.
fn handoff_note(cwd: &Path, said: &[Option<&str>]) -> Option<String> {
    if !handoff::enabled(&settings()) {
        return None;
    }
    let worktree = handoff::worktree_of(cwd)?;
    let said = said.iter().flatten().map(|text| text.len()).sum();
    handoff::launch_note(&worktree, said)
}

/// What the project's memory has to tell an agent as it starts in `cwd`,
/// with the words of its command as what it was asked, unless the config
/// turns memory off: Claude Code, Codex, or another agent crystal knows
/// that's given a first prompt to say it in.
fn remembered(socket: &Path, cwd: &Path, command: &[String]) -> Option<String> {
    let reader = match agents::program_name(command)? {
        "claude" => memory::Reader::Claude,
        "codex" => memory::Reader::Agent,
        _ if catalog::first_prompt_at(command).is_some() => memory::Reader::Agent,
        _ => return None,
    };
    if !memory::enabled_now() {
        return None;
    }
    let asked = command[1..].join(" ");
    launch_memory(socket, cwd, &asked, reader)
}

/// What the memory of the project `cwd` is in tells `reader` as it starts
/// there, asked `asked`: what has to do with the files its worktree has
/// changed comes first.
fn launch_memory(socket: &Path, cwd: &Path, asked: &str, reader: memory::Reader) -> Option<String> {
    let project = memory::project_of(cwd);
    let changed = git::branch_changes(cwd).unwrap_or_default();
    let embedder = embed::shared_now();
    let embedder = embed::as_embed(&embedder);
    memory::for_launch(socket, &project, asked, &changed, reader, embedder)
        .ok()
        .flatten()
}

/// What gives a Claude Code session in `cwd` the tools that search its
/// project's memory: crystal's MCP server, run by `crystal`, the path of
/// this program, and its tools allowed, so it searches without asking for a
/// shell command. Nothing for another program, or with memory off.
fn memory_tools(socket: &Path, cwd: &Path, command: &[String], crystal: &Path) -> Vec<String> {
    if agents::program_name(command) != Some("claude") || !memory::enabled_now() {
        return Vec::new();
    }
    vec![
        "--mcp-config".to_string(),
        mcp::config(crystal, socket, cwd),
        "--allowedTools".to_string(),
        mcp::TOOLS.join(","),
    ]
}

/// Loads the embedding model, when the config says to search with it, and
/// gives every entry without a vector one.
fn embed_waiting(socket: &Path) {
    if !memory::enabled_now() {
        return;
    }
    let Some(embedder) = embed::shared_now() else {
        return;
    };
    match memory::Store::open(socket).and_then(|mut store| store.embed_missing(&*embedder)) {
        Ok(0) => {}
        Ok(count) => eprintln!("crystal daemon: embedded {count} entries of memory"),
        Err(err) => eprintln!("crystal daemon: couldn't embed memory's entries: {err:#}"),
    }
}

/// The arguments each of a task's runs gives Claude: the task's own; in its
/// system prompt, the notes its worktree's sessions left and, with memory
/// on, what its project remembers that has to do with its prompt; and with
/// memory on, crystal's MCP server, with its tools allowed, to search the
/// rest.
fn task_args(socket: &Path, cwd: &Path, spec: &protocol::TaskSpec) -> Vec<String> {
    let memory_on = memory::enabled_now();
    let remembered = memory_on
        .then(|| launch_memory(socket, cwd, &spec.prompt, memory::Reader::Task))
        .flatten();
    let handoff = handoff_note(cwd, &[remembered.as_deref()]);
    let args = agents::with_instructions(&spec.args, &notes(None, handoff, remembered));
    if !memory_on {
        return args;
    }
    let Ok(crystal) = std::env::current_exe() else {
        return args;
    };
    let server = mcp::config(&crystal, socket, cwd);
    let args = agents::with_value(&args, &["--mcp-config"], &server);
    let tools = mcp::TOOLS.join(",");
    agents::with_value(&args, &["--allowedTools", "--allowed-tools"], &tools)
}

/// Starts a task and adds it to `sessions`, its runs adding to
/// `spending`. With `run_prompt`, Claude runs its prompt now; without, the
/// task waits at rest for a follow-up. Given a `conversation`, its runs
/// carry that on.
fn start_task(
    sessions: &mut Vec<Session>,
    socket: &Path,
    spending: &Arc<Spending>,
    task: NewTask,
    conversation: Option<String>,
    run_prompt: bool,
) -> Result<String> {
    let NewTask {
        name,
        cwd,
        spec,
        env,
        backlog,
    } = task;
    ensure!(
        exists("claude", &cwd, env.get("PATH")),
        "command not found: claude"
    );
    let taken = |name: &str| sessions.iter().any(|session| session.name == name);
    let name = match name {
        Some(name) => {
            check_name(&name, taken)?;
            name
        }
        None => {
            let from_prompt = settings()
                .name_from_prompt
                .then_some(spec.prompt.as_str())
                .and_then(names::from_prompt);
            unique_name(from_prompt.as_deref().unwrap_or("task"), taken)
        }
    };

    let id = new_id();
    let env = env::for_session(&env, &name, &id, socket);
    let prompt = spec.prompt.clone();
    let args = task_args(socket, &cwd, &spec);
    let spending = spending.clone();
    let mut session = Session::task(
        id,
        name.clone(),
        spec,
        args,
        cwd,
        env,
        spending,
        conversation,
    );
    // It closes itself when its run ends, from Claude's answer.
    if tasks::enabled(&settings()) {
        session.give_task(new_task_info(prompt.clone(), true, backlog));
    }
    if run_prompt {
        session.prompt(&prompt)?;
    } else {
        session.came_back();
    }
    sessions.push(session);
    Ok(name)
}

/// A task just made, open, and numbered by [`Daemon::number_tasks`].
fn new_task_info(goal: String, background: bool, backlog: Option<u64>) -> TaskInfo {
    TaskInfo {
        id: None,
        goal,
        background,
        backlog,
        waiting: false,
        created: now_seconds(),
        outcome: None,
    }
}

/// The user's settings, read again each time so that a change counts at
/// once. A file that can't be read leaves the defaults.
fn settings() -> Config {
    Config::load().unwrap_or_default()
}

/// Now, in seconds since the Unix epoch.
fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Refuses a name a session can't have: an empty one, one with spaces, or
/// one another session has.
fn check_name(name: &str, taken: impl Fn(&str) -> bool) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.contains(char::is_whitespace),
        "a session name can't be empty or contain spaces"
    );
    ensure!(!taken(name), "a session named {name} already exists");
    Ok(())
}

/// A new session's id. The time it's made, to the nanosecond, sets it
/// apart from the sessions of any daemon before this one; the count sets
/// it apart from this daemon's own, however close together they start.
fn new_id() -> String {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let count = MADE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:x}-{count}")
}

/// The distiller reading what a session did, until it's dropped: while it
/// is, another pass over the same session can't start.
struct Reading {
    reading: Arc<Mutex<HashSet<String>>>,
    id: String,
}

impl Reading {
    /// Marks the session `id` as being read, unless it is already.
    fn start(reading: &Arc<Mutex<HashSet<String>>>, id: &str) -> Option<Reading> {
        if !reading.lock().unwrap().insert(id.to_string()) {
            return None;
        }
        Some(Reading {
            reading: reading.clone(),
            id: id.to_string(),
        })
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.reading.lock().unwrap().remove(&self.id);
    }
}

/// The session a request names by its `id`, from a program in it, or else
/// by its `name`.
fn id_or_name(
    sessions: &mut [Session],
    id: Option<String>,
    name: Option<String>,
) -> Result<&mut Session> {
    match (id, name) {
        (Some(id), _) => with_id(sessions, &id),
        (None, Some(name)) => named(sessions, &name),
        (None, None) => bail!("say which session"),
    }
}

/// The session `handle` names: the one working on task `t12`, or else the
/// one called that.
fn handled_session<'a>(sessions: &'a mut [Session], handle: &str) -> Option<&'a mut Session> {
    let by_task = tasks::parse_id(handle).and_then(|id| {
        sessions
            .iter()
            .position(|session| session.task_id() == Some(id))
    });
    let index = by_task.or_else(|| sessions.iter().position(|session| session.name == handle))?;
    Some(&mut sessions[index])
}

/// The session working on the task `handle` names, by the task's number or
/// the session's name.
fn task_session<'a>(sessions: &'a mut [Session], handle: &str) -> Result<&'a mut Session> {
    handled_session(sessions, handle)
        .with_context(|| format!("there's no task or session called {handle}"))
}

fn named<'a>(sessions: &'a mut [Session], name: &str) -> Result<&'a mut Session> {
    sessions
        .iter_mut()
        .find(|session| session.name == name)
        .with_context(|| format!("no session named {name}"))
}

fn with_id<'a>(sessions: &'a mut [Session], id: &str) -> Result<&'a mut Session> {
    sessions
        .iter_mut()
        .find(|session| session.id == id)
        .with_context(|| format!("no session with id {id}"))
}

/// Shows a session to a client until either of them goes: first the screen
/// as it is (after its history, with `with_history`), then the output as
/// it comes, while the client's keys and size go to the session.
fn attach(
    conn: &UnixStream,
    mut input: BufReader<&UnixStream>,
    found: Found,
    (rows, cols): (u16, u16),
    with_history: bool,
) -> Result<()> {
    let Found { name, id, term } = found;
    term.resize(rows, cols)?;
    let watch = term.watch(with_history);
    let running = watch.feed.is_some();
    protocol::send(conn, &Response::Attached { name, id, running })?;
    let mut output = conn.try_clone()?;
    output.write_all(&watch.screen)?;
    match watch.feed {
        Some(feed) => {
            thread::spawn(move || {
                for chunk in feed {
                    if output.write_all(&chunk).is_err() {
                        break;
                    }
                }
                // The session has ended: the client sees the end of the
                // output and goes.
                let _ = output.shutdown(Shutdown::Write);
            });
        }
        None => conn.shutdown(Shutdown::Write)?,
    }

    while let Ok(Some(frame)) = protocol::recv_frame(&mut input) {
        match frame {
            Frame::Input(keys) => {
                let _ = term.write(&keys);
            }
            Frame::Resize { rows, cols } => term.resize(rows, cols)?,
        }
    }
    term.unwatch(watch.id);
    Ok(())
}

/// What to tell a crystal of another version than this daemon's. Starting
/// the daemon again from that crystal, run as `crystal` runs on this daemon
/// ([`socket::crystal_for`]), makes the two match.
fn version_mismatch(daemon: &str, client: Option<&str>, crystal: &str) -> String {
    let client = match client {
        Some(version) => format!("crystal {version}"),
        None => "an older crystal".to_string(),
    };
    format!(
        "this is {client}, but the daemon is crystal {daemon}: \
         run `{crystal} restart-server` to restart the daemon on this crystal"
    )
}

impl From<anyhow::Error> for Response {
    fn from(err: anyhow::Error) -> Response {
        Response::Error {
            message: format!("{err:#}"),
        }
    }
}

/// Whether `program` names a file to run: a path, taken from `cwd`, or a
/// name found on the client's `PATH`.
fn exists(program: &str, cwd: &Path, path: Option<&String>) -> bool {
    if program.contains('/') {
        return cwd.join(program).is_file();
    }
    let Some(path) = path else {
        return false;
    };
    std::env::split_paths(path).any(|dir| dir.join(program).is_file())
}

/// The program's name, with `-2`, `-3`… added until it's free.
fn unique_name(program: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = Path::new(program)
        .file_name()
        .map(|name| name.to_string_lossy().replace(char::is_whitespace, "-"))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "session".into());
    (1..)
        .map(|n| match n {
            1 => base.clone(),
            n => format!("{base}-{n}"),
        })
        .find(|name| !taken(name))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mismatch_says_both_versions_and_the_way_out() {
        assert_eq!(
            version_mismatch("0.1.0", Some("0.2.0"), "crystal"),
            "this is crystal 0.2.0, but the daemon is crystal 0.1.0: \
             run `crystal restart-server` to restart the daemon on this crystal"
        );
        assert!(
            version_mismatch("0.1.0", None, "crystal").starts_with("this is an older crystal,")
        );
        assert!(
            version_mismatch("0.1.0", None, "crystal --server work")
                .contains("run `crystal --server work restart-server`")
        );
    }

    #[test]
    fn crystal_s_notes_say_where_they_come_from_then_the_task_then_the_memory() {
        let notes = notes(
            Some("about the task".into()),
            Some("handed off".into()),
            Some("remembered".into()),
        );
        assert_eq!(
            notes,
            [
                agents::ABOUT_CRYSTAL,
                "about the task",
                "handed off",
                "remembered"
            ]
        );
    }

    #[test]
    fn with_nothing_to_say_there_are_no_notes_at_all() {
        assert!(notes(None, None, None).is_empty());
        assert_eq!(notes(None, None, Some("remembered".into())).len(), 2);
        assert_eq!(notes(None, Some("handed off".into()), None).len(), 2);
    }

    #[test]
    fn a_name_comes_from_the_program() {
        assert_eq!(unique_name("/usr/local/bin/claude", |_| false), "claude");
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        let taken = ["claude", "claude-2"];
        assert_eq!(
            unique_name("claude", |name| taken.contains(&name)),
            "claude-3"
        );
    }

    #[test]
    fn a_program_without_a_usable_name_falls_back() {
        assert_eq!(unique_name("/", |_| false), "session");
    }

    #[test]
    fn a_name_has_to_be_free_and_one_word() {
        let taken = |name: &str| name == "claude";
        assert!(check_name("reviewer", taken).is_ok());
        assert!(check_name("claude", taken).is_err());
        assert!(check_name("", taken).is_err());
        assert!(check_name("two words", taken).is_err());
    }

    #[test]
    fn no_two_sessions_share_an_id() {
        let ids: Vec<String> = (0..100).map(|_| new_id()).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len());
    }
}
