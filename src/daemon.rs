//! The background process that owns every session. It outlives the
//! terminal it was started from, so sessions keep running when the
//! client goes away.

use crate::agents;
use crate::backlog;
use crate::codex;
use crate::config::{Config, MemorySettings};
use crate::db::Db;
use crate::distill::{self, Job};
use crate::embed;
use crate::env;
use crate::flow_run::{self, Ended, FlowRun, Next, RunState, StepState};
use crate::flows;
use crate::git;
use crate::mcp;
use crate::memory;
use crate::notify::{self, Notice};
use crate::plugin_hooks::{self, Event, Hooks};
use crate::project;
use crate::protocol::{
    self, Activity, AgentEvent, Backlog, Conversation, Frame, NewSession, NewTask, PendingTask,
    Request, Response, TaskInfo, TaskOutcome, TaskRecord, TaskSpec, TaskStart, TaskState, TaskView,
};
use crate::session::{STOP_GRACE, Session, Term};
use crate::skill;
use crate::socket;
use crate::spending::Spending;
use crate::state::SavedSession;
use crate::tasks;
use crate::typing;
use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufReader, ErrorKind, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{fs, process, thread};

/// How often the daemon reads every session's screen for what its agent is
/// doing, tells the user about the sessions that need them, and writes
/// down the sessions that are running.
const KEEP_UP_EVERY: Duration = Duration::from_millis(250);

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
    let daemon = Arc::new(Daemon {
        socket: socket.to_path_buf(),
        db: Mutex::new(db),
        sessions: Mutex::default(),
        flows: Mutex::default(),
        spending: Arc::new(Spending::new(Db::open(socket)?)),
        hooks: Hooks::new(socket),
        distilling: Arc::default(),
        preparing: Arc::default(),
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
    /// The plugins' hooks, told what happens.
    hooks: Hooks,
    /// The sessions the distiller is reading now, by id: one pass at a
    /// time over each.
    distilling: Arc<Mutex<HashSet<String>>>,
    /// Getting the model that searches memory by meaning ready: what's
    /// being done, and why it last failed.
    preparing: Arc<Mutex<Preparing>>,
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
            let message = version_mismatch(&ours, incoming.version.as_deref());
            return Ok(protocol::send(&conn, &Response::Error { message })?);
        }
        let request = incoming.request()?;
        if let Request::Attach {
            name,
            rows,
            cols,
            history,
        } = request
        {
            return match self.find(name.as_deref()) {
                Ok(found) => attach(&conn, input, found, (rows, cols), history),
                Err(err) => Ok(protocol::send(&conn, &Response::from(err))?),
            };
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
                    start(&mut sessions, &self.socket, new, saved.conversation)
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
    /// doing, and writes down the running sessions when they've changed.
    fn keep_up(&self) {
        let mut last_saved: Vec<SavedSession> = Vec::new();
        // Whether each session was running, and what its agent was doing,
        // the last time round, by id, to tell plugins what changed.
        let mut last_seen: HashMap<String, (bool, Option<protocol::Activity>)> = HashMap::new();
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
                let now = (session.is_running(), session.activity());
                // Every session starts running, its agent doing nothing it
                // has said; that it started was told as it did.
                let started = (true, None);
                let before = last_seen.insert(session.id.clone(), now).unwrap_or(started);
                for change in plugin_hooks::session_changes(before, now) {
                    self.hooks
                        .tell(Event::about_session(change, &session.info()));
                }
            }
            last_seen.retain(|id, _| sessions.iter().any(|session| &session.id == id));
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
            run.interrupt();
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
            let Some(ended) = how_step_ended(sessions, run, step) else {
                continue;
            };
            let next = run.step_ended(step, ended);
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
    /// or has the session of the step at its gate wait on the user.
    fn carry_out(&self, sessions: &mut Vec<Session>, run: &mut FlowRun, next: Next) {
        match next {
            Next::Run { step, prompt } => {
                if let Err(err) = self.start_step(sessions, run, step, &prompt) {
                    run.could_not_start(step, format!("{err:#}"));
                }
            }
            Next::Gate(step) => {
                if let Some(session) = step_session(sessions, run, step) {
                    session.on_agent_event(AgentEvent::Asking);
                }
            }
            Next::Finished | Next::Stopped => {}
        }
    }

    /// Runs `step` of `run`, asking it `prompt`. While the step's session is
    /// there at rest, it's a follow-up there, in the same conversation. An
    /// ended one makes way for a new task, which carries its conversation
    /// on; with none, the step starts in a new task of its own.
    fn start_step(
        &self,
        sessions: &mut Vec<Session>,
        run: &mut FlowRun,
        step: usize,
        prompt: &str,
    ) -> Result<()> {
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
                // Not a task: a session that has taken the name since.
                (false, _) => {}
            }
        }
        let cwd = self.step_dir(run, step)?;
        let taken = |name: &str| sessions.iter().any(|session| session.name == name);
        let name = unique_name(&run.session_name(step), taken);
        let task = NewTask {
            name: Some(name),
            cwd,
            spec: run.task_spec(step, prompt),
            env: run.env.clone(),
            backlog: None,
        };
        let name = start_task(
            sessions,
            &self.socket,
            &self.spending,
            task,
            conversation,
            true,
        )?;
        // As a task, it's the step, in the project's history and memory,
        // rather than the whole of its prompt.
        let session = sessions.last_mut().expect("start_task added it");
        if session.task_record().is_some() {
            let goal = format!("{} {}: {}", run.name, run.step_name(step), run.goal);
            session.give_task(new_task_info(goal, true, None));
            self.number_tasks(std::slice::from_mut(session));
        }
        run.steps[step].session = Some(name);
        Ok(())
    }

    /// Where `step` of `run` runs: where the run started, or, from the
    /// first step that wants one, the worktree the run makes for itself
    /// then.
    fn step_dir(&self, run: &mut FlowRun, step: usize) -> Result<PathBuf> {
        if !run.wants_worktree(step) {
            return Ok(run.cwd.clone());
        }
        if let Some(worktree) = &run.worktree {
            return Ok(worktree.clone());
        }
        let (worktree, branch) = git::add_new_worktree(&run.cwd, &run.branch())?;
        // The plugins that listen for new worktrees hear of it, as they do
        // of one a client makes.
        let made = Event::about_worktree(true, &worktree, Some(&branch));
        self.hooks.tell(made);
        run.worktree = Some(worktree.clone());
        Ok(worktree)
    }

    /// Starts a run of the flow called `flow` in the config file, on `goal`.
    fn start_flow(
        &self,
        flow: &str,
        goal: String,
        cwd: PathBuf,
        env: BTreeMap<String, String>,
    ) -> Result<String> {
        let config = settings();
        flows::ensure_enabled(&config)?;
        let Some(found) = config.flows.iter().find(|found| found.name == flow) else {
            let known: Vec<&str> = config.flows.iter().map(|f| f.name.as_str()).collect();
            if known.is_empty() {
                bail!(
                    "there's no flow called {flow}, nor any other: `crystal flow example` shows one"
                );
            }
            bail!(
                "there's no flow called {flow}; there's {}",
                known.join(", ")
            );
        };
        let mut sessions = self.sessions.lock().unwrap();
        let mut runs = self.flows.lock().unwrap();
        let name = flow_run::new_name(flow, &runs);
        let mut run = FlowRun::new(
            name.clone(),
            found.clone(),
            &config.profiles,
            goal,
            cwd,
            env,
            now_seconds(),
        );
        let next = run.start();
        self.carry_out(&mut sessions, &mut run, next);
        runs.push(run);
        flow_run::forget_old(&mut runs);
        Ok(name)
    }

    /// Changes the run called `name` with `change`, then does what that
    /// leads to.
    fn change_flow(
        &self,
        name: &str,
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
        if let Some(session) = gate.and_then(|step| step_session(&mut sessions, run, step)) {
            session.on_agent_event(AgentEvent::Started);
        }
        self.carry_out(&mut sessions, run, next);
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
        self.write_down(session.cwd(), task);
        self.distill_later(session, task);
    }

    /// Writes a task that has just closed, which ran in `cwd`, into its
    /// project's history, and keeps how it went in the project's memory.
    /// When it was done and was for a backlog item, ticks the item.
    fn write_down(&self, cwd: &Path, task: &TaskRecord) {
        let project = project::of(cwd).path;
        {
            let mut db = self.db.lock().unwrap();
            if let Err(err) = db.record_task(&project, task) {
                eprintln!("crystal daemon: couldn't write down a closed task: {err:#}");
            }
            let done = task.state() == TaskState::Done;
            let ticks = done && backlog::enabled(&settings());
            if let (true, Some(number)) = (ticks, task.backlog) {
                let ticked =
                    db.change_backlog(&project, |store| store.mark(number, true, now_seconds()));
                if let Err(err) = ticked {
                    eprintln!("crystal daemon: couldn't tick #{number} on the backlog: {err:#}");
                }
            }
        }
        self.hooks.tell(Event::task_closed(cwd, task));
        self.remember_outcome(cwd, task);
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
        thread::spawn(move || {
            let name = &job.session;
            match distill::run(&job) {
                Ok(report) => {
                    eprintln!("crystal daemon: distilled {name}: {}", report.line());
                    for why in &report.rejected {
                        eprintln!("crystal daemon:   rejected {why}");
                    }
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
        Ok(Response::Distilled(report?))
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
        if let Err(err) = kept {
            eprintln!("crystal daemon: couldn't remember how a task went: {err:#}");
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
    /// before any typing, which takes a moment.
    fn running_term(&self, name: &str) -> Result<Arc<Term>> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = named(&mut sessions, name)?;
        ensure!(session.is_running(), "{name} has ended");
        Ok(session.term())
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
                Ok(self.started(&mut sessions, name))
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
            } => {
                let mut sessions = self.sessions.lock().unwrap();
                let session = match id {
                    Some(id) => with_id(&mut sessions, &id)?,
                    None => named(&mut sessions, &name)?,
                };
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
                session.on_agent_event(event);
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
                if session.is_running() {
                    self.hooks
                        .tell(Event::about_session("session.ended", &session.info()));
                }
                session.stop();
                Ok(Response::Done)
            }
            Request::Worktree {
                path,
                branch,
                created,
            } => {
                let event = Event::about_worktree(created, &path, branch.as_deref());
                self.hooks.tell(event);
                Ok(Response::Done)
            }
            Request::Rename { name, new_name } => {
                let mut sessions = self.sessions.lock().unwrap();
                if new_name != name {
                    check_name(&new_name, |taken| {
                        sessions.iter().any(|session| session.name == taken)
                    })?;
                }
                named(&mut sessions, &name)?.name = new_name.clone();
                // A flow's step keeps to its session under the new name.
                for run in self.flows.lock().unwrap().iter_mut() {
                    for step in &mut run.steps {
                        if step.session.as_ref() == Some(&name) {
                            step.session = Some(new_name.clone());
                        }
                    }
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
            } => {
                tasks::ensure_enabled(&settings())?;
                let mut sessions = self.sessions.lock().unwrap();
                let session = match (id, name) {
                    (Some(id), _) => with_id(&mut sessions, &id)?,
                    (None, Some(name)) => named(&mut sessions, &name)?,
                    (None, None) => bail!("say which session's task to close"),
                };
                let state = if failed {
                    TaskState::Failed
                } else {
                    TaskState::Done
                };
                let closed = session.close_task(state, &summary)?;
                self.write_down_closed(session, &closed);
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
                    entries: memory::stale_last(found, &project),
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
                Ok(Response::TaskAdded { id })
            }
            Request::StartTask { id, env } => self.start_pending(id, env),
            Request::ShowTask { task } => {
                tasks::ensure_enabled(&settings())?;
                Ok(Response::Task(self.find_task(&task)?))
            }
            Request::CancelTask { task } => {
                self.cancel_task(&task)?;
                Ok(Response::Done)
            }
            Request::TaskLog { task } => {
                tasks::ensure_enabled(&settings())?;
                let task = self.find_task(&task)?;
                let transcript = self.transcript_of(&task);
                Ok(Response::TaskLog { task, transcript })
            }
            Request::Answer {
                task,
                answer,
                message,
            } => {
                let mut sessions = self.sessions.lock().unwrap();
                task_session(&mut sessions, &task)?.answer(answer, message.as_deref())?;
                Ok(Response::Done)
            }
            Request::Interrupt { task } => {
                let mut sessions = self.sessions.lock().unwrap();
                task_session(&mut sessions, &task)?.interrupt()?;
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
                let number =
                    self.change_backlog(&dir, |store| store.add(&text, tags, now_seconds()))?;
                Ok(Response::Added { number })
            }
            Request::BacklogMark { dir, number, done } => {
                self.change_backlog(&dir, |store| store.mark(number, done, now_seconds()))?;
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
                self.change_flow(&run, FlowRun::approve)?;
                Ok(Response::Done)
            }
            Request::SendFlowBack { run, notes } => {
                self.change_flow(&run, |run| run.send_back(&notes))?;
                Ok(Response::Done)
            }
            Request::RetryFlow { run } => {
                self.change_flow(&run, FlowRun::retry)?;
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
                        self.write_down(session.cwd(), &cancelled);
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
        tasks
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
        self.write_down(&pending.cwd, &cancelled);
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
                start(&mut sessions, &self.socket, new, None)
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
        Ok(self.started(&mut sessions, name))
    }

    fn new_session(&self, new: NewSession) -> Result<Response> {
        let mut sessions = self.sessions.lock().unwrap();
        let name = start(&mut sessions, &self.socket, new, None)?;
        Ok(self.started(&mut sessions, name))
    }

    /// What's said once the session called `name` has started: the plugins
    /// are told, its task is numbered, and the client is told both.
    fn started(&self, sessions: &mut [Session], name: String) -> Response {
        self.number_tasks(sessions);
        self.tell_started(sessions, &name);
        let session = sessions.iter().find(|session| session.name == name);
        let task = session.and_then(Session::task_id);
        Response::Created { name, task }
    }

    /// Tells the plugins that the session called `name` has started.
    fn tell_started(&self, sessions: &[Session], name: &str) {
        if let Some(session) = sessions.iter().find(|session| session.name == name) {
            self.hooks
                .tell(Event::about_session("session.started", &session.info()));
        }
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
                start(&mut sessions, &self.socket, new, launch.conversation)
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
        self.tell_started(&sessions, name);
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

/// How the run of `step` ended, once it has: from what its task's run came
/// to, or a failure when its session has gone.
fn how_step_ended(sessions: &[Session], run: &FlowRun, step: usize) -> Option<Ended> {
    let name = run.steps[step].session.as_ref()?;
    let Some(session) = sessions.iter().find(|session| session.name == *name) else {
        return Some(Ended {
            failed: true,
            answer: format!("its session, {name}, has gone"),
            cost_usd: 0.0,
        });
    };
    let result = session.finished_run()?;
    Some(Ended {
        failed: result.failed || !session.is_running(),
        answer: result.text,
        cost_usd: result.cost_usd,
    })
}

/// A session found for a client about to show it.
struct Found {
    name: String,
    id: String,
    term: Arc<Term>,
}

/// Starts a session and adds it to `sessions`. Given a `conversation`, an
/// agent that can pick one up starts back in it.
fn start(
    sessions: &mut Vec<Session>,
    socket: &Path,
    new: NewSession,
    conversation: Option<Conversation>,
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
    let taken = |name: &str| sessions.iter().any(|session| session.name == name);
    let name = match name {
        Some(name) => {
            check_name(&name, taken)?;
            name
        }
        None => unique_name(program, taken),
    };

    let id = new_id();
    let rollouts = codex::Rollouts::for_session(&command, &cwd, &env);
    let env = env::for_session(&env, &name, &id, socket);
    let crystal = std::env::current_exe()?;
    // A conversation that can't be picked up any more is left behind: the
    // agent starts a new one, which its hooks or its rollout will name.
    let conversation = conversation.filter(Conversation::can_resume);
    let resume = conversation
        .as_ref()
        .map(|conversation| conversation.id.as_str());
    let mut asked = command.clone();
    // What it was started to do: a conversation picked up again has been
    // asked that already, whether tasks are on or off.
    let given_task = task.clone();
    // With tasks off, a session started with something to do is just a
    // session.
    let config = settings();
    let task = task.filter(|_| tasks::enabled(&config));
    let mut about_task = None;
    if let Some(goal) = &task {
        let paragraph = tasks::instructions(backlog::enabled(&config));
        // Codex has no system prompt to add to, so it hears it at the end
        // of what it's asked to do.
        if agents::program_name(&command) == Some("codex")
            && let Some(last) = asked.last_mut()
            && last == goal
        {
            last.push_str("\n\n");
            last.push_str(&notes(Some(paragraph.clone()), None).join("\n\n"));
        }
        about_task = Some(paragraph);
    }
    let instructions = notes(about_task, remembered(socket, &cwd, &command));
    let argv = agents::argv(
        &asked,
        &crystal,
        resume,
        given_task.as_deref(),
        &instructions,
    );
    let argv = agents::with_options(argv, &memory_tools(socket, &cwd, &command, &crystal));
    let mut session = Session::spawn(id, name.clone(), command, &argv, cwd, &env)?;
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

/// What crystal tells an agent on top of what it was asked, a paragraph
/// each: about its task first, the one thing it mustn't forget, then what
/// the project's memory has, all opened by where they come from. Nothing at
/// all when there's nothing to say.
fn notes(about_task: Option<String>, remembered: Option<String>) -> Vec<String> {
    let said: Vec<String> = about_task.into_iter().chain(remembered).collect();
    if said.is_empty() {
        return said;
    }
    let mut notes = vec![agents::ABOUT_CRYSTAL.to_string()];
    notes.extend(said);
    notes
}

/// What the project's memory has to tell a Claude Code session as it
/// starts, with the words of its command as what it was asked, unless the
/// config turns memory off. Codex has no way to be told something at
/// launch without it showing as the user's own first message, so it isn't.
fn remembered(socket: &Path, cwd: &Path, command: &[String]) -> Option<String> {
    if agents::program_name(command) != Some("claude") {
        return None;
    }
    if !memory::enabled_now() {
        return None;
    }
    let project = memory::project_of(cwd);
    let embedder = embed::shared_now();
    let asked = command[1..].join(" ");
    memory::for_launch(socket, &project, &asked, false, embed::as_embed(&embedder))
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

/// The arguments each of a task's runs gives Claude: the task's own, and,
/// with memory on, what its project remembers that has to do with its
/// prompt, in its system prompt, and crystal's MCP server, with its tools
/// allowed, to search the rest.
fn task_args(socket: &Path, cwd: &Path, spec: &protocol::TaskSpec) -> Vec<String> {
    if !memory::enabled_now() {
        return spec.args.clone();
    }
    let project = memory::project_of(cwd);
    let embedder = embed::shared_now();
    let embedder = embed::as_embed(&embedder);
    let remembered = memory::for_launch(socket, &project, &spec.prompt, true, embedder)
        .ok()
        .flatten();
    let args = agents::with_instructions(&spec.args, &notes(None, remembered));
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
        None => unique_name("task", taken),
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
/// the daemon again from that crystal makes the two match.
fn version_mismatch(daemon: &str, client: Option<&str>) -> String {
    let client = match client {
        Some(version) => format!("crystal {version}"),
        None => "an older crystal".to_string(),
    };
    format!(
        "this is {client}, but the daemon is crystal {daemon}: \
         run `crystal restart-server` to restart the daemon on this crystal"
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
            version_mismatch("0.1.0", Some("0.2.0")),
            "this is crystal 0.2.0, but the daemon is crystal 0.1.0: \
             run `crystal restart-server` to restart the daemon on this crystal"
        );
        assert!(version_mismatch("0.1.0", None).starts_with("this is an older crystal,"));
    }

    #[test]
    fn crystal_s_notes_say_where_they_come_from_then_the_task_then_the_memory() {
        let notes = notes(Some("about the task".into()), Some("remembered".into()));
        assert_eq!(
            notes,
            [agents::ABOUT_CRYSTAL, "about the task", "remembered"]
        );
    }

    #[test]
    fn with_nothing_to_say_there_are_no_notes_at_all() {
        assert!(notes(None, None).is_empty());
        assert_eq!(notes(None, Some("remembered".into())).len(), 2);
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
