//! The background process that owns every session. It outlives the
//! terminal it was started from, so sessions keep running when the
//! client goes away.

use crate::agent_rules;
use crate::agents;
use crate::artifacts;
use crate::backlog;
use crate::catalog;
use crate::codex;
use crate::config::{Config, MemorySettings};
use crate::db;
use crate::db::Db;
use crate::distill::{self, Job};
use crate::embed;
use crate::env;
use crate::event_log::{self, Bus, Subscription};
use crate::events::{Event, Filter, Kind, Since};
use crate::flow_run::{self, Ended, FlowRun, Next, Place, RunState, StepState};
use crate::flows;
use crate::front;
use crate::git::{self, Checkout};
use crate::handoff;
use crate::handover::{self, Gate, Ticket};
use crate::layout::{Layout, Order};
use crate::layout_relay::{NoTui, Relay};
use crate::mcp;
use crate::memory;
use crate::messages::{self, Sender};
use crate::names;
use crate::notify::{self, Notice};
use crate::plugin_hooks;
use crate::printable;
use crate::project;
use crate::protocol::{
    self, Activity, AgentEvent, ArchivedSession, Artifact, ArtifactKind, Backlog, Conversation,
    Frame, Front, NewSession, NewTask, PendingTask, Request, Response, SessionInfo, State,
    TaskBrief, TaskInfo, TaskOutcome, TaskRecord, TaskSpec, TaskStart, TaskState, TaskView,
    Worktree,
};
use crate::report;
use crate::session::{Change, STOP_GRACE, Session, Term, signal_group};
use crate::skill;
use crate::socket;
use crate::spending::Spending;
use crate::state::{self, SavedSession};
use crate::task;
use crate::tasks;
use crate::typing;
use crate::vt;
use anyhow::{Context, Result, anyhow, bail, ensure};
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::io::{BufReader, ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsFd, AsRawFd, RawFd};
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

/// How often the keep-up loop looks for agents that have sat idle for too
/// long: the settings are read each time. `CRYSTAL_IDLE_CHECK_MS` sets it
/// otherwise, for tests.
fn idle_check_every() -> Duration {
    std::env::var("CRYSTAL_IDLE_CHECK_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .map_or(Duration::from_secs(15), Duration::from_millis)
}

/// How often a client that only listens is checked for having hung up.
const LOOK_FOR_HANG_UP: Duration = Duration::from_secs(1);

/// How many rows of a session's history, above its screen, `wait --output`
/// looks through: enough for what scrolled off between two looks.
const HISTORY_MATCHED: usize = 200;

/// The least time between two looks at a screen for `wait --output`, so a
/// program writing without a break doesn't keep the daemon looking.
const LOOK_AT_MOST_EVERY: Duration = Duration::from_millis(50);

/// How long a handover waits for the requests the daemon is answering, and
/// for plugins' hooks and the distiller, before it stops them.
const HANDOVER_GRACE: Duration = Duration::from_secs(3);

/// Runs the daemon on `socket`, or with `handover`, the descriptor of what
/// the last daemon handed over as it ran this crystal in its place (see
/// [`crate::handover`]), carries on from there.
pub fn run(socket: &Path, handover: Option<RawFd>) -> Result<()> {
    // Leave the client's terminal, so closing it doesn't hang up the
    // daemon. Fails harmlessly when run in the foreground from a shell, and
    // in a daemon handed over, which left it already.
    // SAFETY: setsid has no preconditions.
    unsafe {
        libc::setsid();
    }
    let handed = handover.map(|fd| handover::read(fd).unwrap_or_else(|err| give_up(socket, err)));
    // The sessions handed over, or started again, keep the history the
    // settings say.
    keep_scrollback();
    // Before listening: a daemon that can't keep its state doesn't start,
    // rather than run with none and write over it.
    let db = match Db::open(socket) {
        Ok(db) => db,
        Err(err) if handed.is_some() => give_up(socket, err),
        Err(err) => return Err(err),
    };
    let listener = match &handed {
        Some(handed) => match handover::inherit(handed.listener) {
            Ok(listener) => UnixListener::from(listener),
            Err(err) => give_up(socket, err.into()),
        },
        None => listen(socket)?,
    };
    let events = Arc::new(Bus::new(socket));
    let hooks = plugin_hooks::follow(&events, socket);
    // A rules file of the user's that can't be used is said in the log,
    // each time it's read.
    agent_rules::log_problems();
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
        listener: listener.as_raw_fd(),
        gate: Arc::default(),
        asking_to_hand_over: Mutex::default(),
        db: Mutex::new(db),
        sessions: Mutex::default(),
        flows: Mutex::default(),
        spending: Arc::new(Spending::new(Db::open(socket)?)),
        events,
        distilling: Arc::default(),
        preparing: Arc::default(),
        handoff: Mutex::default(),
        layout: Relay::new(),
        sends: messages::Guard::default(),
        projects: Mutex::default(),
    });
    // A daemon starts again after every upgrade, or is handed over to the
    // new crystal, so this is where the skill an earlier crystal installed
    // learns this one's commands: before the saved sessions start, so that
    // Claude Code in them reads the new one.
    match skill::refresh() {
        Ok(Some(path)) => eprintln!("crystal daemon: updated the skill in {}", path.display()),
        Ok(None) => {}
        Err(err) => eprintln!("crystal daemon: couldn't update the skill: {err:#}"),
    }
    let cold = handed.is_none();
    // The sessions written down start again before the daemon answers,
    // but for the agents after the first, which wait their turns.
    let first = match handed {
        Some(handed) => {
            daemon.take_over(handed);
            daemon.start_waiting(false)
        }
        None => {
            daemon.start_saved_sessions();
            let first = daemon.start_waiting(false);
            daemon.take_up_flows();
            first
        }
    };
    // Those start a moment apart, on a thread of their own, while the
    // daemon answers. Once they're back, or carried on, a daemon handed
    // over to starts up as any other does.
    thread::spawn({
        let daemon = daemon.clone();
        let hooks = hooks.clone();
        move || {
            let (back, mut failed) = daemon.start_waiting(true);
            let back = first.0 + back;
            failed.splice(0..0, first.1);
            if !failed.is_empty() {
                eprintln!(
                    "crystal daemon: started {back} sessions again; {} couldn't start: {}",
                    failed.len(),
                    failed.join(", ")
                );
            }
            if cold && back + failed.len() > 0 {
                let version = protocol::version();
                daemon.events.emit(Event::restarted(&version, back, failed));
            }
            hooks.start_up();
        }
    });
    // With search by meaning on, the models are got ready now, rather than
    // when a session starts: downloaded if they aren't here (unless
    // `CRYSTAL_NO_MODEL_DOWNLOAD` is set, as in crystal's tests), loaded, and
    // every entry without a vector given one.
    if memory::enabled_now() && settings().memory.embeddings {
        daemon.prepare_embeddings(std::env::var_os("CRYSTAL_NO_MODEL_DOWNLOAD").is_none());
    }
    thread::spawn({
        let daemon = daemon.clone();
        move || daemon.keep_up()
    });
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        // Once a handover has begun, the daemon takes no more connections:
        // they wait for the next crystal.
        let Some((conn, ticket)) = daemon.gate.admit(conn) else {
            loop {
                thread::park();
            }
        };
        daemon.answer(conn, ticket);
    }
    Ok(())
}

/// Stops a crystal that can't take over from the daemon that handed over
/// to it, the way a daemon that crashed does: what it inherited closes with
/// it, which hangs up on those programs, and the client starts the next
/// daemon, which starts them again from the database. The socket goes
/// first, so the client finds it gone.
fn give_up(socket: &Path, err: anyhow::Error) -> ! {
    eprintln!("crystal daemon: couldn't take over: {err:#}");
    let _ = fs::remove_file(socket);
    process::exit(1);
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
    /// The listening socket's descriptor, which a handover keeps open.
    listener: RawFd,
    /// Every connection comes in through here, so that a handover knows
    /// which it has to answer first.
    gate: Arc<Gate>,
    /// The connections that asked for the handover underway, for the next
    /// crystal to answer.
    asking_to_hand_over: Mutex<Vec<UnixStream>>,
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
    /// The TUIs that take layout commands, which go to the one used last.
    layout: Relay,
    /// How many messages each session has sent others in the last minute,
    /// which `crystal send` holds to a most.
    sends: messages::Guard,
    /// What the daemon knows of the projects crystal keeps a list of.
    /// Taken after `sessions` and before `db`.
    projects: Mutex<KnownProjects>,
}

/// The projects the sessions run in that are on the list already, as
/// this daemon has put them there, and each listed project's repository,
/// found once: reading which branch it's on is cheap after that.
#[derive(Default)]
struct KnownProjects {
    listed: HashSet<PathBuf>,
    checkouts: HashMap<PathBuf, Option<Checkout>>,
}

/// What [`Daemon::prepare_embeddings`] is doing, or why it failed.
#[derive(Debug, Default)]
struct Preparing {
    doing: Option<&'static str>,
    failed: Option<String>,
}

impl Daemon {
    /// Answers a connection the gate let in, on a thread of its own.
    fn answer(self: &Arc<Self>, conn: UnixStream, ticket: Ticket) {
        let daemon = self.clone();
        thread::spawn(move || {
            if let Err(err) = daemon.serve(conn, ticket) {
                eprintln!("crystal daemon: {err:#}");
            }
        });
    }

    /// Answers the request on `conn`, which `ticket` counts in at the gate
    /// until it's answered: a handover waits for that.
    fn serve(&self, conn: UnixStream, ticket: Ticket) -> Result<()> {
        let mut input = BufReader::new(&conn);
        let Some(incoming) = protocol::recv_request(&mut input)? else {
            return Ok(());
        };
        // A shutdown and a handover go through whatever the versions: they
        // are how a crystal of another version gets this daemon out of its
        // way, or has it run that version.
        let ours = protocol::version();
        let any_version = incoming.is_shutdown() || incoming.is_handover();
        if incoming.version.as_deref() != Some(ours.as_str()) && !any_version {
            let crystal = socket::crystal_for(&self.socket);
            let message = version_mismatch(&ours, incoming.version.as_deref(), &crystal);
            return Ok(protocol::send(&conn, &Response::Error { message })?);
        }
        let request = incoming.request()?;
        // These take the connection over, and answer as they go. A handover
        // cuts them, and their clients come back.
        match request {
            Request::Attach {
                name,
                rows,
                cols,
                history,
            } => {
                drop(ticket);
                return match self.find(name.as_deref()) {
                    Ok(found) => attach(&conn, input, found, (rows, cols), history),
                    Err(err) => Ok(protocol::send(&conn, &Response::from(err))?),
                };
            }
            Request::Subscribe { filter, since } => {
                drop(ticket);
                return self.stream_events(&conn, filter, since);
            }
            Request::TakeLayoutOrders { used } => {
                drop(ticket);
                return self.layout.serve(&conn, input, used);
            }
            Request::WaitOutput {
                name,
                pattern,
                timeout_ms,
            } => {
                drop(ticket);
                let timeout = timeout_ms.map(Duration::from_millis);
                return self.wait_for_output(&conn, &name, &pattern, timeout);
            }
            Request::Handover { exe, format } => {
                return self.hand_over(&conn, ticket, &exe, format);
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

    /// Puts the sessions that were running when the last daemon stopped
    /// without being asked to, because it crashed or the machine rebooted,
    /// back in the list in their places, each waiting its turn to start
    /// again: see [`Daemon::start_waiting`].
    fn start_saved_sessions(&self) {
        let mut sessions = self.sessions.lock().unwrap();
        let saved = self.db.lock().unwrap().sessions();
        let saved = saved.unwrap_or_else(|err| {
            eprintln!("crystal daemon: couldn't read the sessions to start again: {err:#}");
            Vec::new()
        });
        sessions.extend(
            saved
                .into_iter()
                .map(|saved| Session::to_start(new_id(), saved)),
        );
    }

    /// Starts the sessions waiting their turn to start again, in the list's
    /// order, each in its place and from this daemon's environment: an
    /// agent `[sessions] restart_spacing_ms` after the agent before it, so
    /// they don't all start at once, any other program straight away; or
    /// without `wait`, only those that needn't wait, and the first agent.
    /// One that can't start stays where it was, failed, saying why. Gives
    /// back how many started, and the names of those that couldn't.
    fn start_waiting(&self, wait: bool) -> (usize, Vec<String>) {
        let spacing = settings().sessions.restart_spacing();
        let env = env::current();
        let mut back = 0;
        let mut failed = Vec::new();
        // The first agent waits for nothing: it's started straight away,
        // before the daemon answers, so it needn't wait after it either.
        let mut agent_started = wait.then(Instant::now);
        loop {
            let mut sessions = self.sessions.lock().unwrap();
            let wait_for =
                agent_started.map_or(Duration::ZERO, |at| spacing.saturating_sub(at.elapsed()));
            let next = sessions.iter().position(|session| {
                session.is_starting() && (wait_for.is_zero() || !starts_an_agent(&session.launch()))
            });
            let Some(index) = next else {
                if !wait || !sessions.iter().any(Session::is_starting) {
                    break;
                }
                // Not held meanwhile: the daemon answers, and the list may
                // change, so the next is looked for again.
                drop(sessions);
                thread::sleep(wait_for);
                continue;
            };
            let saved = sessions[index].launch();
            let agent = starts_an_agent(&saved);
            match self.start_in_place(&mut sessions, index, saved, env.clone()) {
                Ok(_) => back += 1,
                Err(_) => failed.push(sessions[index].name.clone()),
            }
            if agent {
                agent_started = Some(Instant::now());
            }
        }
        (back, failed)
    }

    /// Starts the session at `index`, yet to start again, from `saved`, in
    /// the environment `env`, keeping its place in the list and its id, and
    /// tells of it. One that can't start stays there, failed, saying why,
    /// and that's told instead.
    fn start_in_place(
        &self,
        sessions: &mut Vec<Session>,
        index: usize,
        saved: SavedSession,
        env: BTreeMap<String, String>,
    ) -> Result<String> {
        // It makes way, so that its name is free for the session started.
        let mut waiting = sessions.remove(index);
        let id = waiting.id.clone();
        match self.start_saved(sessions, saved.clone(), env, Some(id)) {
            Ok(name) => {
                // Whoever was looking at the one waiting is let go, to look
                // again at the session started under its id.
                waiting.term().close();
                // `start` adds the session at the end; it goes where the
                // one waiting was.
                let mut started = sessions.pop().expect("start added a session");
                // A step of a flow run waiting at its gate waits on the user
                // again, as after any restart.
                let runs = self.flows.lock().unwrap();
                if runs.iter().any(|run| waits_at_gate(run, &name)) {
                    started.on_agent_event(AgentEvent::Asking);
                }
                drop(runs);
                sessions.insert(index, started);
                let info = sessions[index].info();
                self.events
                    .emit(Event::about_session(Kind::SessionStarted, &info));
                Ok(name)
            }
            Err(err) => {
                let why = format!("{err:#}");
                eprintln!("crystal daemon: couldn't start {} again: {why}", saved.name);
                let failed = if waiting.is_starting() {
                    waiting.fail_to_start(&why);
                    waiting
                } else {
                    // One that had failed already says why afresh, under an
                    // id of its own, so that whoever shows it looks again.
                    Session::failed_to_start(new_id(), saved, &why)
                };
                self.events.emit(Event::start_failed(&failed.info()));
                sessions.insert(index, failed);
                Err(err)
            }
        }
    }

    /// Starts a session again from what was written down of it, from the
    /// environment `env`, at the end of `sessions`, under the id `id` or a
    /// new one, and gives back its name: an agent in its conversation, a
    /// task at rest, any other program from the start. It comes back with
    /// its task as it was, closed or not.
    fn start_saved(
        &self,
        sessions: &mut Vec<Session>,
        saved: SavedSession,
        env: BTreeMap<String, String>,
        id: Option<String>,
    ) -> Result<String> {
        let id = id.unwrap_or_else(new_id);
        let goal = saved.goal.clone();
        let backlog = goal.as_ref().and_then(|goal| goal.backlog);
        let brief = goal
            .as_ref()
            .map(|goal| goal.brief.clone())
            .unwrap_or_default();
        let name = match saved.task {
            // A task comes back at rest: a run it was in the middle of
            // can't be picked up halfway, so it isn't run again either.
            Some(spec) => {
                let task = NewTask {
                    name: Some(saved.name),
                    cwd: saved.cwd,
                    spec,
                    env,
                    backlog,
                    brief,
                };
                let conversation = saved.conversation.map(|conversation| conversation.id);
                start_task_as(
                    id,
                    sessions,
                    &self.socket,
                    &self.spending,
                    task,
                    conversation,
                    false,
                )?
            }
            None => {
                let new = NewSession {
                    name: Some(saved.name),
                    cwd: saved.cwd,
                    command: saved.command,
                    env,
                    task: goal.as_ref().map(|goal| goal.goal.clone()),
                    backlog,
                    brief,
                };
                start_as(
                    id,
                    sessions,
                    &self.socket,
                    new,
                    saved.conversation,
                    saved.resume,
                )?
            }
        };
        if let (Some(goal), Some(session)) = (goal, sessions.last_mut()) {
            session.give_task(goal);
        }
        Ok(name)
    }

    /// Stops the session called `name` and keeps it in the archive, out of
    /// the list. Written down before it stops: a session that couldn't be
    /// kept isn't stopped. Its open task is cancelled, as a kill does, but
    /// it's kept open, to be open again when it starts again.
    fn archive(&self, name: &str) -> Result<Response> {
        let mut sessions = self.sessions.lock().unwrap();
        let index = sessions
            .iter()
            .position(|session| session.name == name)
            .with_context(|| format!("no session named {name}"))?;
        let info = sessions[index].info();
        let archived = ArchivedSession {
            id: info.id.clone(),
            session: sessions[index].launch(),
            worktree: info.worktree.clone(),
            archived: now_seconds(),
        };
        self.db.lock().unwrap().archive(&archived)?;
        let mut session = sessions.remove(index);
        let cancelled = tasks::enabled(&settings())
            .then(|| session.cancel_task("its session was archived"))
            .flatten();
        if let Some(cancelled) = cancelled {
            self.write_down_closed(&session, &cancelled);
        }
        session.stop();
        if info.state == State::Running {
            self.events.emit(Event::ended(&info, "archived".into()));
        }
        self.events
            .emit(Event::about_session(Kind::SessionArchived, &info));
        Ok(Response::Done)
    }

    /// Starts the session archived under `name` again, under that name or
    /// the next one free, and takes it out of the archive. One that can't
    /// start, say because its directory has gone, stays in it.
    fn unarchive(&self, name: &str, env: BTreeMap<String, String>) -> Result<Response> {
        let mut sessions = self.sessions.lock().unwrap();
        let db = self.db.lock().unwrap();
        let mut archived = db
            .unarchive(name)?
            .with_context(|| format!("no session named {name} in the archive"))?;
        let name = archived.name().to_string();
        let taken = |name: &str| sessions.iter().any(|session| session.name == name);
        if taken(&name) {
            let free = (2..)
                .map(|number| format!("{name}-{number}"))
                .find(|name| !taken(name))
                .expect("some number is free");
            archived.session.name = free;
        }
        let started = match self.start_saved(&mut sessions, archived.session.clone(), env, None) {
            Ok(started) => started,
            Err(err) => {
                db.archive(&archived)?;
                return Err(err);
            }
        };
        drop(db);
        Ok(self.started(&mut sessions, started, Kind::TaskOpened))
    }

    /// Again and again: reads every session's screen for what its agent is
    /// doing, tells what has changed, and writes down the running sessions
    /// when they've changed.
    fn keep_up(&self) {
        let mut last_saved: Vec<SavedSession> = Vec::new();
        // The sessions whose program has ended and been told of, by id: a
        // daemon handed ones that had ended was told of them already.
        let mut told_ended: HashSet<String> = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|session| !session.is_running() && !session.is_unstarted())
            .map(|session| session.id.clone())
            .collect();
        let mut last_runs: Vec<FlowRun> = Vec::new();
        let mut idle_checked = Instant::now();
        let idle_check_every = idle_check_every();
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
                session.check_model();
                self.tell_changes(session);
                for closed in session.take_closed() {
                    self.write_down_closed(session, &closed);
                }
            }
            // Before telling the user anything: a step the flow goes on
            // from needs nobody, and a gate needs them.
            self.follow_flows(&mut sessions);
            if idle_checked.elapsed() >= idle_check_every {
                idle_checked = Instant::now();
                stop_idle_agents(&mut sessions);
            }
            // Read only when a session has something to tell, at most once
            // a round.
            let mut read = None;
            let mut after = || {
                *read.get_or_insert_with(|| {
                    Duration::from_secs(notify::settings().notifications.after_secs)
                })
            };
            for session in sessions.iter_mut() {
                if let Some(notice) = session.notice(&mut after) {
                    notify::tell(notice, &self.socket);
                }
                self.tell_changes(session);
                // One yet to start again hasn't ended, and keeps its id once
                // it has started.
                let ended = !session.is_running() && !session.is_unstarted();
                if ended && told_ended.insert(session.id.clone()) {
                    let info = session.info();
                    let status = info.status();
                    self.events.emit(Event::ended(&info, status));
                }
            }
            told_ended.retain(|id| sessions.iter().any(|session| &session.id == id));
            self.list_projects_of(&sessions);
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

    /// Puts the projects `sessions` run in on the list of those crystal
    /// knows, those this daemon hasn't put there already.
    fn list_projects_of(&self, sessions: &[Session]) {
        let mut known = self.projects.lock().unwrap();
        for project in sessions.iter().filter_map(Session::project_path) {
            if known.listed.contains(project) {
                continue;
            }
            match self.db.lock().unwrap().list_project(project, true) {
                Ok(()) => {
                    known.listed.insert(project.to_path_buf());
                }
                Err(err) => eprintln!(
                    "crystal daemon: couldn't keep {} in the list of projects: {err:#}",
                    project.display()
                ),
            }
        }
    }

    /// The projects crystal knows, by their main worktrees, as they are
    /// now. One whose repository has gone is left out, but stays listed:
    /// a disk not mounted today may be back tomorrow.
    fn known_projects(&self) -> Result<Vec<Worktree>> {
        let paths = self.db.lock().unwrap().listed_projects()?;
        let mut known = self.projects.lock().unwrap();
        let mut projects = Vec::new();
        for path in paths {
            if !path.join(".git").exists() {
                known.checkouts.remove(&path);
                continue;
            }
            let checkout = known
                .checkouts
                .entry(path.clone())
                .or_insert_with(|| Checkout::find(&path));
            let worktree = checkout.as_ref().map(Checkout::worktree);
            if let Some(worktree) = worktree.filter(|w| w.main && w.path == path) {
                projects.push(worktree);
            }
        }
        Ok(projects)
    }

    /// Puts the project `dir` is in on the list, or takes it off: not while
    /// a session is in it, which would put it back.
    fn list_project(&self, dir: &Path, listed: bool) -> Result<Response> {
        let checkout = Checkout::find(dir)
            .with_context(|| format!("{} isn't in a git repository", dir.display()))?;
        let project = checkout.project_path().to_path_buf();
        let sessions = self.sessions.lock().unwrap();
        if !listed {
            let in_it = sessions
                .iter()
                .filter(|session| session.project_path() == Some(project.as_path()))
                .count();
            ensure!(
                in_it == 0,
                "{} has {in_it} session{} in it: kill {} first",
                project.display(),
                if in_it == 1 { "" } else { "s" },
                if in_it == 1 { "it" } else { "them" },
            );
        }
        let mut known = self.projects.lock().unwrap();
        self.db.lock().unwrap().list_project(&project, listed)?;
        if listed {
            known.checkouts.insert(project.clone(), Some(checkout));
        } else {
            known.listed.remove(&project);
            known.checkouts.remove(&project);
        }
        Ok(Response::Done)
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
            take_up(&mut sessions, run);
        }
        *self.flows.lock().unwrap() = runs;
    }

    /// Hands the daemon over to the crystal at `exe` (see
    /// [`crate::handover`]), which answers `conn`, the connection `ticket`
    /// let in, once it has taken over. Refused, the daemon says why, and
    /// carries on as it was: the client restarts it cold. Comes back with
    /// nothing else: a handover that fails once it has begun stops the
    /// daemon.
    fn hand_over(&self, conn: &UnixStream, ticket: Ticket, exe: &Path, format: u32) -> Result<()> {
        if let Err(err) = handover::check(exe, format) {
            let message = format!("couldn't hand over: {err:#}");
            return Ok(protocol::send(conn, &Response::Error { message })?);
        }
        let first = {
            let mut asking = self.asking_to_hand_over.lock().unwrap();
            asking.push(conn.try_clone()?);
            asking.len() == 1
        };
        // Counted in until now, so that one asked for at the same time is
        // in the list before the first goes on.
        drop(ticket);
        // One handover at a time: the next crystal answers this one too.
        if !first {
            return Ok(());
        }
        eprintln!("crystal daemon: handing over to {}", exe.display());
        handover::begin();
        let deadline = Instant::now() + HANDOVER_GRACE;
        let waiting = self.gate.close(&self.socket, deadline);
        let Err(err) = self.exec_handed_over(&waiting, exe, deadline);
        // The sessions can't carry on: the daemon stops as a shutdown that
        // keeps them does, and the client starts the next, which starts them
        // again. Its socket goes first, so the client finds it gone.
        eprintln!("crystal daemon: couldn't hand over, so it stops: {err:#}");
        let _ = fs::remove_file(&self.socket);
        let message = format!("couldn't hand over: {err:#}");
        for conn in self.asking_to_hand_over.lock().unwrap().iter() {
            let _ = protocol::send(
                conn,
                &Response::Error {
                    message: message.clone(),
                },
            );
        }
        process::exit(1);
    }

    /// Gets every session ready to hand over, then runs the crystal at `exe`
    /// in this process: comes back only with why it couldn't. `waiting` are
    /// the connections the next crystal answers.
    fn exec_handed_over(
        &self,
        waiting: &[UnixStream],
        exe: &Path,
        deadline: Instant,
    ) -> Result<Infallible> {
        let mut sessions = self.sessions.lock().unwrap();
        let flows = self.flows.lock().unwrap();
        // What has happened so far is told, and a task that closed written
        // down, before the sessions go.
        for session in sessions.iter_mut() {
            for closed in session.take_closed() {
                self.write_down(session.cwd(), Some(&session.info()), &closed);
            }
            self.tell_changes(session);
        }
        // Written down first: whatever goes wrong from here, the next
        // daemon starts them again, as after any restart.
        let saved: Vec<SavedSession> = sessions.iter().filter_map(Session::saved).collect();
        {
            let mut db = self.db.lock().unwrap();
            db.save_sessions(&saved)?;
            db.save_flow_runs(&flows)?;
        }
        handover::HELPERS.finish(deadline);
        handover::stop_reading();
        let mut handed = Vec::new();
        // Held until the exec: see [`Session::hand_over`].
        let mut held = Vec::new();
        for session in sessions.iter() {
            let (session, state) = session.hand_over()?;
            handed.push(session);
            held.push(state);
        }
        let keep = |conns: &[UnixStream]| {
            conns
                .iter()
                .map(|conn| handover::keep_across_exec(conn.as_fd()))
                .collect::<std::io::Result<Vec<RawFd>>>()
        };
        // SAFETY: the listener is open for as long as the daemon runs.
        let listener = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.listener) };
        let asking = self.asking_to_hand_over.lock().unwrap();
        let state = handover::State {
            from: protocol::version(),
            listener: handover::keep_across_exec(listener)?,
            asking: keep(&asking)?,
            waiting: keep(waiting)?,
            sessions: handed,
            flows: flows.iter().map(handover::HandedFlow::of).collect(),
        };
        let dir = self.socket.parent().unwrap_or(Path::new("/"));
        let file = handover::write(dir, &state)?;
        let state = handover::keep_across_exec(file.as_fd())?;
        // No write to the database is halfway through as the exec closes
        // it.
        let _db = self.db.lock().unwrap();
        let _spending = self.spending.hold();
        let _events = self.events.hold();
        let err = handover::exec(exe, &self.socket, state);
        Err(err).with_context(|| format!("couldn't run {}", exe.display()))
    }

    /// Carries on from the daemon that handed over: its sessions and flow
    /// runs as they were. A session that can't be carried on starts again,
    /// as after any restart. Then the clients that asked for the handover
    /// are told, and the connections that came in meanwhile are answered.
    fn take_over(self: &Arc<Self>, handed: handover::State) {
        let handover::State {
            from,
            asking,
            waiting,
            sessions: handed_sessions,
            flows,
            ..
        } = handed;
        let mut sessions = self.sessions.lock().unwrap();
        let mut again = Vec::new();
        for handed in handed_sessions {
            let name = handed.name().to_string();
            let saved = handed.saved();
            let processes = handed.processes();
            match Session::adopt(handed, &self.spending) {
                Ok(session) => sessions.push(session),
                Err(err) => {
                    eprintln!("crystal daemon: couldn't carry {name} on: {err:#}");
                    // Hung up on, then reaped, since they're this process's
                    // children still.
                    for pid in processes {
                        signal_group(pid, libc::SIGHUP);
                        thread::spawn(move || handover::reap(pid));
                    }
                    again.extend(saved);
                }
            }
        }
        let carried = sessions.len();
        let restarted: Vec<String> = again.iter().map(|saved| saved.name.clone()).collect();
        // They wait their turn to start again, as after any restart.
        sessions.extend(
            again
                .into_iter()
                .map(|saved| Session::to_start(new_id(), saved)),
        );
        let mut flows: Vec<FlowRun> = flows
            .into_iter()
            .map(handover::HandedFlow::taken_over)
            .collect();
        // A run whose step's session started again goes on as after any
        // restart.
        for run in &mut flows {
            let step = run
                .current()
                .and_then(|step| run.steps[step].session.as_ref());
            if step.is_some_and(|name| restarted.contains(name)) {
                take_up(&mut sessions, run);
            }
        }
        *self.flows.lock().unwrap() = flows;
        drop(sessions);
        eprintln!("crystal daemon: took over from crystal {from}: {carried} sessions carried on");
        self.events
            .emit(Event::handed_over(&from, &protocol::version(), carried));
        for conn in asking {
            if let Ok(conn) = handover::inherit(conn) {
                let answer = Response::HandedOver { sessions: carried };
                let _ = protocol::send(UnixStream::from(conn), &answer);
            }
        }
        for conn in waiting {
            let conn = handover::inherit(conn).map(UnixStream::from);
            if let Some((conn, ticket)) = conn.ok().and_then(|conn| self.gate.admit(conn)) {
                self.answer(conn, ticket);
            }
        }
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
                let session = run.steps[step].session.clone();
                let notice = Notice {
                    session: session.clone().unwrap_or_default(),
                    activity: Activity::Waiting,
                    text: format!("{} failed at {}", run.name, run.step_name(step)),
                    jump: session,
                    agent: None,
                };
                notify::tell(notice, &self.socket);
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
                brief: TaskBrief::default(),
            };
            start(sessions, &self.socket, new, None, None)?
        } else {
            let task = NewTask {
                name: Some(name),
                cwd: cwd.clone(),
                spec: run.task_spec(step, prompt),
                env: run.env.clone(),
                backlog: None,
                brief: TaskBrief::default(),
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
            session.give_task(new_task_info(goal, !terminal, None, TaskBrief::default()));
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
                // Not fetched: the daemon mustn't wait on the network
                // here, so it's `origin`'s branch as the last fetch left it.
                let base = git::Base {
                    named: None,
                    configured: settings().worktrees.base,
                    fetch: false,
                };
                let (worktree, branch) = git::add_new_worktree(&run.cwd, &run.slug(), &base)?;
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
    /// if it had started, into its project's history, and tells of it.
    /// When it was done and was for a backlog item, ticks the item.
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
        // A handover underway would only stop it halfway.
        if handover::underway() {
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
        let on_disk = embed::models_dir().map_or(0, |dir| embed::on_disk(&dir));
        let (entries, embedded) = memory::Store::open(&self.socket)?.counts(embed::MODEL)?;
        if embed::is_loaded() && embedded < entries {
            self.prepare_embeddings(true);
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

    /// Gets the models that search memory by meaning ready, on a thread of
    /// their own, unless that's being done already: downloads them if they
    /// aren't here and `download` says to, then, while the config still says
    /// to search with them, loads them, lets go of other models' vectors and
    /// gives every entry its vector.
    fn prepare_embeddings(&self, download: bool) {
        {
            let mut preparing = self.preparing.lock().unwrap();
            if preparing.doing.is_some() {
                return;
            }
            *preparing = Preparing {
                doing: Some("downloading the models"),
                failed: None,
            };
        }
        let preparing = self.preparing.clone();
        let socket = self.socket.clone();
        thread::spawn(move || {
            let doing = |what| preparing.lock().unwrap().doing = Some(what);
            let prepared = (|| -> Result<()> {
                let downloaded = embed::models_dir().is_some_and(|dir| embed::is_downloaded(&dir));
                if !downloaded {
                    if !download {
                        return Ok(());
                    }
                    eprintln!("crystal daemon: downloading {}", embed::names());
                    embed::download(false)?;
                }
                doing("loading the models");
                let Some(models) = embed::shared_now() else {
                    return Ok(());
                };
                doing("embedding the entries");
                let mut store = memory::Store::open(&socket)?;
                store.forget_vectors_but(embed::MODEL)?;
                match store.embed_missing(&*models)? {
                    0 => {}
                    count => eprintln!("crystal daemon: embedded {count} entries of memory"),
                }
                Ok(())
            })();
            let mut preparing = preparing.lock().unwrap();
            preparing.doing = None;
            if let Err(err) = prepared {
                eprintln!("crystal daemon: couldn't get memory's models ready: {err:#}");
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

    /// Types `text` into the session called `name`, and presses Enter after
    /// it with `enter`; a task takes it as a follow-up, another run that
    /// carries its conversation on. Sent from another session, the one
    /// with id `from`, it's tidied, says which session sent it, and counts
    /// toward what that session may send in a minute. An agent asking the
    /// user something takes nothing, unless `force` says to type it anyway.
    fn send(
        &self,
        name: &str,
        text: &str,
        enter: bool,
        from: Option<&str>,
        force: bool,
    ) -> Result<Response> {
        // In a block of its own, so the sessions are let go before the
        // typing below, which takes a moment.
        let (text, info, sender) = {
            let mut sessions = self.sessions.lock().unwrap();
            // A sender the daemon doesn't know, say one killed since, sends
            // as a script would.
            let sender = from.and_then(|id| {
                let session = sessions.iter().find(|session| session.id == id)?;
                let info = session.info();
                Some(Sender::new(&info.id, &info.name, info.task.as_ref()))
            });
            let text = match &sender {
                Some(_) => messages::tidy(text)?,
                None => text.to_string(),
            };
            let session = named(&mut sessions, name)?;
            ensure!(session.is_running(), "{name} has ended");
            if let Some(sender) = &sender {
                ensure!(
                    sender.id != session.id,
                    "{name} is this session: a session can't send a message to itself"
                );
            }
            if let Some(why) = session.blocked().filter(|_| !force) {
                bail!(messages::blocked(name, &why, session.is_task()));
            }
            // Counted only once it can go, so a refused send costs nothing.
            if let Some(sender) = &sender {
                self.sends.admit(&sender.id, Instant::now())?;
            }
            let text = match &sender {
                Some(sender) => messages::compose(sender, &text),
                None => text,
            };
            if session.is_task() {
                let prompted = session.prompt(&text);
                let info = session.info();
                drop(sessions);
                return self.sent(prompted, &info, sender.as_ref(), &text);
            }
            (text, session.info(), sender)
        };
        let typed = self.running_term(name).and_then(|term| {
            term.write(&typing::keystrokes(&text, term.wants_bracketed_paste()))?;
            if enter {
                thread::sleep(typing::ENTER_PAUSE);
                term.write(typing::ENTER)?;
            }
            Ok(())
        });
        self.sent(typed, &info, sender.as_ref(), &text)
    }

    /// What came of sending `text` to the session `info` is about, from
    /// `sender`: sent, it's an event; not sent after all, the sender has it
    /// back from what it may send in a minute.
    fn sent(
        &self,
        sent: Result<()>,
        info: &SessionInfo,
        sender: Option<&Sender>,
        text: &str,
    ) -> Result<Response> {
        if let Err(err) = sent {
            if let Some(sender) = sender {
                self.sends.give_back(&sender.id);
            }
            return Err(err);
        }
        self.events.emit(Event::message(info, sender, text));
        Ok(Response::Done)
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
                agent,
                cwd,
                subagent,
                model,
            } => {
                let agent = agent.unwrap_or_else(|| "claude".to_string());
                let mut sessions = self.sessions.lock().unwrap();
                let id = match id {
                    Some(id) => id,
                    None => named(&mut sessions, &name)?.id.clone(),
                };
                let Some(id) = reporting_session(
                    &sessions,
                    &agent,
                    &id,
                    conversation.as_ref(),
                    cwd.as_deref(),
                ) else {
                    return Ok(Response::Done);
                };
                // A background task knows what its Claude does from
                // Claude's own events, not hooks the user installed.
                if with_id(&mut sessions, &id)?.is_task() {
                    return Ok(Response::Done);
                }
                // Hooks in an agent's own settings run for an agent that
                // the one in front started too, which isn't the session's.
                if !with_id(&mut sessions, &id)?.reports_for(&agent) {
                    return Ok(Response::Done);
                }
                // Only Claude Code's and Codex's Stop hooks take the answer
                // that keeps the agent from ending its turn.
                let can_remind = matches!(agent.as_str(), "claude" | "codex");
                if let Some(prompt) = prompt {
                    self.name_from_prompt(&mut sessions, &id, &prompt);
                }
                let session = with_id(&mut sessions, &id)?;
                if let Some(conversation) = conversation {
                    session.set_hooked_conversation(&agent, conversation);
                }
                if let Some(model) = model {
                    session.heard_model(&model);
                }
                // A conversation just named is looked at now: what's
                // written to it from here on is news.
                session.check_model();
                // Reminded that its task is open, the agent carries on: its
                // turn hasn't ended, and it isn't done.
                if event == AgentEvent::TurnEnded
                    && can_remind
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
                    let kind = match event {
                        AgentEvent::SubagentStarted => Some(Kind::SubagentStarted),
                        AgentEvent::SubagentStopped => Some(Kind::SubagentStopped),
                        _ => None,
                    };
                    if let (Some(kind), Some(subagent)) = (kind, subagent) {
                        self.events
                            .emit(Event::subagent(kind, &session.info(), subagent));
                    }
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
            Request::ReportMetadata { id, name, metadata } => {
                let mut sessions = self.sessions.lock().unwrap();
                let session = match (id, name) {
                    (Some(id), _) => with_id(&mut sessions, &id)?,
                    (None, Some(name)) => named(&mut sessions, &name)?,
                    (None, None) => bail!("say which session the report is about"),
                };
                ensure!(session.is_running(), "{} has ended", session.name);
                session.take_metadata(&metadata)?;
                Ok(Response::Done)
            }
            Request::Kill { name } => {
                // An archived session is killed by taking it out of the
                // archive.
                let running =
                    (self.sessions.lock().unwrap().iter()).any(|session| session.name == name);
                if !running {
                    let gone = self.db.lock().unwrap().unarchive(&name)?;
                    ensure!(gone.is_some(), "no session named {name}");
                    return Ok(Response::Done);
                }
                self.kill(&name)?;
                Ok(Response::Done)
            }
            Request::Emit { event } => {
                self.events.emit(*event);
                Ok(Response::Done)
            }
            Request::Notify { text, id, name } => {
                ensure!(!text.trim().is_empty(), "say what to tell the user");
                let mut sessions = self.sessions.lock().unwrap();
                let session = match (id, name) {
                    (Some(id), _) => Some(with_id(&mut sessions, &id)?.name.clone()),
                    (None, Some(name)) => Some(named(&mut sessions, &name)?.name.clone()),
                    (None, None) => None,
                };
                drop(sessions);
                let notice = Notice {
                    text: match &session {
                        Some(session) => format!("{session}: {text}"),
                        None => text,
                    },
                    session: session.clone().unwrap_or_default(),
                    activity: Activity::Waiting,
                    jump: session,
                    agent: None,
                };
                notify::tell(notice, &self.socket);
                Ok(Response::Done)
            }
            Request::Projects => Ok(Response::Projects {
                projects: self.known_projects()?,
            }),
            Request::AddProject { dir } => self.list_project(&dir, true),
            Request::RemoveProject { dir } => self.list_project(&dir, false),
            Request::Subscribe { .. }
            | Request::WaitOutput { .. }
            | Request::Handover { .. }
            | Request::TakeLayoutOrders { .. } => {
                bail!("this takes the connection over")
            }
            Request::Layout(order) => {
                let mut layout = match self.layout.pass(order.clone()) {
                    Err(err) if err.is::<NoTui>() => self.lay_out_alone(order)?,
                    passed => passed?,
                };
                // A TUI knows only whether its own terminal has the focus.
                layout.presence = notify::presence();
                Ok(Response::Layout(layout))
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
            Request::Archive { name } => self.archive(&name),
            Request::Archived => Ok(Response::Archived {
                sessions: self.db.lock().unwrap().archived()?,
            }),
            Request::Unarchive { name, env } => self.unarchive(&name, env),
            Request::DeleteArchived { name } => {
                let gone = self.db.lock().unwrap().unarchive(&name)?;
                ensure!(gone.is_some(), "no session named {name} in the archive");
                Ok(Response::Done)
            }
            Request::Send {
                name,
                text,
                enter,
                from,
                force,
            } => self.send(&name, &text, enter, from.as_deref(), force),
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
            Request::ExplainAgent { name, agent } => {
                let mut sessions = self.sessions.lock().unwrap();
                let session = named(&mut sessions, &name)?;
                let explained = session.explain_screen(agent.as_deref());
                Ok(Response::Explained(Box::new(explained)))
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
                // Work left in the middle of a rebase or a merge isn't
                // done, whatever the summary says.
                if !failed && let Some(what) = session.worktree_in_progress() {
                    bail!(
                        "{}'s worktree is in the middle of {}: finish it or abort it \
                         first, or close the task with `crystal done --failed`",
                        session.info().name,
                        what.what()
                    );
                }
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
                self.prepare_embeddings(true);
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
                    entries: memory::marked(found, &project),
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
            Request::TaskToTerminal { task, env } => self.task_to_terminal(&task, env),
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
            brief,
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
                    brief,
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
                    brief,
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

    /// Turns the background task `handle` names, by its number or its
    /// session's name, into a session in a terminal, from the client's
    /// environment `env`: Claude Code, with the task's own arguments but
    /// those only `claude -p` takes, picking its conversation up, in its
    /// directory, under its name and in its place in the list. Its task
    /// goes on in it as it stands, open or closed, under its number: an
    /// open one is closed with `crystal done` from then on. Its `claude`,
    /// at rest, is let go. Refused while a run is going on, and for a task
    /// with no conversation yet.
    fn task_to_terminal(&self, handle: &str, env: BTreeMap<String, String>) -> Result<Response> {
        let mut sessions = self.sessions.lock().unwrap();
        let name = task_session(&mut sessions, handle)?.name.clone();
        let index = sessions
            .iter()
            .position(|session| session.name == name)
            .expect("it was just found");
        let session = &sessions[index];
        ensure!(
            session.is_task(),
            "{name} isn't a background task: it's in a terminal already"
        );
        ensure!(
            !session.in_a_run(),
            "{name} is in the middle of a run: `crystal wait {name}` for it, or `crystal \
             interrupt {name}`, first"
        );
        let launch = session.launch();
        let id = launch
            .conversation
            .as_ref()
            .map(|conversation| conversation.id.clone())
            .with_context(|| format!("{name} has no conversation to pick up yet"))?;
        // Claude Code in a terminal picks up a conversation it has the
        // transcript of.
        let transcript = crate::distill::transcript_of(&id, session.env()).with_context(|| {
            format!("Claude Code has no transcript of {name}'s conversation, {id}, to pick up")
        })?;
        let conversation = Conversation {
            id,
            transcript: Some(transcript),
            prompted: true,
        };
        let spec = launch.task.clone().expect("a background task has its spec");
        let mut command = vec!["claude".to_string()];
        command.extend(task::terminal_args(&spec.args));
        let goal = launch.goal.clone();
        let backlog = goal.as_ref().and_then(|goal| goal.backlog);
        let brief = goal
            .as_ref()
            .map(|goal| goal.brief.clone())
            .unwrap_or_default();
        // An open task is said as Claude's task, so that it's told how to
        // close it; one closed already is only carried over.
        let open = goal.as_ref().filter(|goal| goal.is_open());
        let new = NewSession {
            name: Some(name.clone()),
            cwd: launch.cwd.clone(),
            command,
            env,
            task: open.map(|goal| goal.goal.clone()),
            backlog,
            brief,
        };
        // The task makes way, and comes back if Claude doesn't start.
        let background = sessions.remove(index);
        if let Err(err) = start(&mut sessions, &self.socket, new, Some(conversation), None) {
            sessions.insert(index, background);
            return Err(err);
        }
        background.stop();
        // `start` adds the session at the end; it goes where the task was,
        // with the task as it stood.
        let mut started = sessions.pop().expect("start added a session");
        if let Some(goal) = goal {
            started.give_task(TaskInfo {
                background: false,
                waiting: false,
                ..goal
            });
        }
        let task_id = started.task_id();
        sessions.insert(index, started);
        let info = sessions[index].info();
        self.events
            .emit(Event::about_session(Kind::SessionStarted, &info));
        Ok(Response::Created {
            name,
            task: task_id,
        })
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
                Change::Bell => Some(Event::about_session(Kind::SessionBell, &info)),
                Change::UnseenCopy => Some(Event::about_session(Kind::SessionCopyDropped, &info)),
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
            term.waiting_for_output(|| matching_row(&term, name, &pattern, timeout, &hung_up))
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

    /// Kills the session called `name` and lets go of it, cancelling its
    /// task if it has one open.
    fn kill(&self, name: &str) -> Result<()> {
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
        Ok(())
    }

    /// Carries out a layout command with no TUI open to: on the tabs as the
    /// TUIs last kept them, the way the TUI would have, and keeps them in
    /// their place, for the next TUI to open with. A tab closed with its
    /// sessions has them killed.
    fn lay_out_alone(&self, order: Order) -> Result<Layout> {
        let sessions = self.sessions.lock().unwrap();
        let infos = sessions.iter().map(Session::info).collect();
        drop(sessions);
        let flows = self.flows.lock().unwrap().clone();
        let db = self.db.lock().unwrap();
        let kept = db.ui(db::TABS)?;
        let alone = crate::tui::obey_alone(infos, flows, kept.as_deref(), order)
            .map_err(|why| anyhow!(why))?;
        db.keep_ui(db::TABS, &alone.tabs)?;
        drop(db);
        for name in &alone.kill {
            self.kill(name)?;
        }
        Ok(alone.layout)
    }

    /// Runs an ended session's command again, in its directory and under
    /// its name, keeping its place in the list. An agent whose conversation
    /// can be picked up starts back in it. One yet to start again after a
    /// restart, or that couldn't, starts now, as the restart would have
    /// started it.
    fn respawn(&self, name: &str, env: BTreeMap<String, String>) -> Result<Response> {
        let mut sessions = self.sessions.lock().unwrap();
        let index = sessions
            .iter()
            .position(|session| session.name == name)
            .with_context(|| format!("no session named {name}"))?;
        ensure!(!sessions[index].is_running(), "{name} is still running");
        if sessions[index].is_unstarted() {
            let saved = sessions[index].launch();
            self.start_in_place(&mut sessions, index, saved, env)?;
            return Ok(Response::Done);
        }

        // The ended session makes way for the new one, and comes back if
        // that doesn't start.
        let ended = sessions.remove(index);
        let launch = ended.launch();
        // Run again, its task is open again: the work goes on.
        let backlog = launch.goal.as_ref().and_then(|goal| goal.backlog);
        let brief = launch
            .goal
            .as_ref()
            .map(|goal| goal.brief.clone())
            .unwrap_or_default();
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
                    brief,
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
                    brief,
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

/// Stops the agents that have sat idle for longer than the settings allow:
/// see [`Session::idle_for`]. They stay in the list, ended, to start again
/// in their conversations.
fn stop_idle_agents(sessions: &mut [Session]) {
    let idle: Vec<(usize, Duration)> = sessions
        .iter()
        .enumerate()
        .filter_map(|(index, session)| Some((index, session.idle_for()?)))
        .collect();
    // The settings are only read when there's an agent they could stop.
    if idle.is_empty() {
        return;
    }
    let Some(limit) = settings().sessions.idle_limit() else {
        return;
    };
    for (index, for_how_long) in idle {
        if for_how_long >= limit {
            sessions[index].stop_idle();
        }
    }
}

/// Takes up `run` as the last daemon left it when it stopped: a step in a
/// terminal whose session came back, or waits its turn to, with its task
/// open carries on there;
/// any other that was running then was cut short, and waits to be run
/// again; one waiting at its gate waits on the user again. Its steps start
/// from this daemon's environment, as the sessions it starts again do.
fn take_up(sessions: &mut [Session], run: &mut FlowRun) {
    let carried_on = run.running().is_some_and(|step| {
        run.in_terminal(step)
            && step_session(sessions, run, step).is_some_and(|session| {
                let open = session
                    .task_record()
                    .is_some_and(|task| task.outcome.is_none());
                (session.is_running() || session.is_starting()) && open
            })
    });
    if !carried_on {
        run.interrupt();
    }
    run.env = env::current();
    let at_gate = run
        .current()
        .filter(|&step| run.steps[step].state == StepState::AtGate);
    if let Some(session) = at_gate.and_then(|step| step_session(sessions, run, step)) {
        session.on_agent_event(AgentEvent::Asking);
    }
}

/// Whether the session called `name` runs the step of `run` waiting at its
/// gate.
fn waits_at_gate(run: &FlowRun, name: &str) -> bool {
    run.current()
        .filter(|&step| run.steps[step].state == StepState::AtGate)
        .is_some_and(|step| run.steps[step].session.as_deref() == Some(name))
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

/// Starts a session and adds it to `sessions`, under an id of its own:
/// see [`start_as`].
fn start(
    sessions: &mut Vec<Session>,
    socket: &Path,
    new: NewSession,
    conversation: Option<Conversation>,
    resume_command: Option<Vec<String>>,
) -> Result<String> {
    start_as(
        new_id(),
        sessions,
        socket,
        new,
        conversation,
        resume_command,
    )
}

/// Starts a session under the id `id` and adds it to `sessions`. Given a
/// `conversation`, an agent that can pick one up starts back in it; given a
/// `resume_command`, the command an agent said resumes it, it's resumed
/// with that instead. Never anywhere but its directory.
fn start_as(
    id: String,
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
        brief,
    } = new;
    let Some(program) = command.first() else {
        bail!("no command to run");
    };
    check_dir(&cwd)?;
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
    let mut env = env::for_session(&env, &name, &id, socket);
    // Claude Code started here gets crystal's hooks with `--settings`.
    if agents::program_name(&asked) == Some("claude") {
        env.insert(agents::HOOKED.into(), "claude".into());
    }
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
    // Its acceptance criteria go under its goal, in its first prompt, when
    // it starts afresh.
    let asked = match &given_task {
        Some(goal) if resume.is_none() && !resumed => {
            tasks::with_criteria_in(asked, goal, &brief.accept)
        }
        _ => asked,
    };
    // With tasks off, a session started with something to do is just a
    // session.
    let task = task.filter(|_| tasks::enabled(&config));
    // The pull request and the issue it's about, which it's told of beside
    // its task, whether tasks are on or off.
    let about_task = paragraphs([
        task.as_ref()
            .map(|_| tasks::instructions(backlog::enabled(&config))),
        tasks::forge_notes(&brief, &cwd),
    ]);
    let parallel = (agents::program_name(&command) == Some("claude"))
        .then(|| agents::PARALLEL_WORK.to_string());
    let remembered = remembered(socket, &cwd, &command);
    let said = [
        task.as_deref(),
        about_task.as_deref(),
        parallel.as_deref(),
        remembered.as_deref(),
    ];
    let handoff = handoff_note(&cwd, &said);
    // Picked up again with its own command, its conversation has heard
    // crystal's notes already.
    let instructions = if resumed {
        Vec::new()
    } else {
        notes(about_task, parallel, handoff, remembered)
    };
    let argv = agents::argv(
        &asked,
        &crystal,
        resume,
        given_task.as_deref(),
        &instructions,
    );
    let argv = agents::with_options(argv, &claude_tools(socket, &cwd, &asked, &crystal, &config));
    let argv = codex::with_instructions(argv, &instructions, codex::home(&env).as_deref());
    keep_scrollback();
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
        session.give_task(new_task_info(goal, false, backlog, brief));
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

/// The paragraphs there are, one after the other, or `None` when there
/// are none.
fn paragraphs<const N: usize>(paragraphs: [Option<String>; N]) -> Option<String> {
    let said: Vec<String> = paragraphs.into_iter().flatten().collect();
    (!said.is_empty()).then(|| said.join("\n\n"))
}

/// What crystal tells an agent on top of what it was asked, a paragraph
/// each: about its task first, the one thing it mustn't forget, then how
/// to work on several things at once here, then the notes the sessions
/// before it in its worktree left, then what the project's memory has, all
/// opened by where they come from. Nothing at all when there's nothing to
/// say.
fn notes(
    about_task: Option<String>,
    parallel: Option<String>,
    handoff: Option<String>,
    remembered: Option<String>,
) -> Vec<String> {
    let said: Vec<String> = about_task
        .into_iter()
        .chain(parallel)
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

/// What gives a Claude Code session in `cwd` the tools crystal tells it
/// to use, allowed up front so it never stops to ask for them: the crystal
/// commands its notes name, and with memory on, crystal's MCP server, run
/// by `crystal`, the path of this program, and its tools. Nothing for
/// another program.
fn claude_tools(
    socket: &Path,
    cwd: &Path,
    command: &[String],
    crystal: &Path,
    config: &Config,
) -> Vec<String> {
    if agents::program_name(command) != Some("claude") {
        return Vec::new();
    }
    let mut tools = crystal_commands(config);
    let mut options = Vec::new();
    if memory::enabled(config) {
        options.extend([
            "--mcp-config".to_string(),
            mcp::config(crystal, socket, cwd),
        ]);
        tools.extend(mcp::TOOLS);
    }
    if !tools.is_empty() {
        options.extend(["--allowedTools".to_string(), tools.join(",")]);
    }
    options
}

/// Claude Code's permission rules for crystal's own commands, so that an
/// agent doing what its notes and crystal's skill teach (starting and
/// driving sessions of its own, reading them, closing its task, noting
/// something for later) doesn't stop for the user at every step: a task in
/// the background that did would sit there with its work done, and one
/// driving workers would wait on each. A plugin's commands only while it's
/// on. What removes or cancels what's there (`crystal kill`, `worktree rm`,
/// `tasks cancel`, `flow cancel`, `backlog rm`, `memory rm`), what's the
/// user's to decide (a flow's gate: `flow approve` and `back`), and what
/// answers another agent's question for it (`send-keys` and `answer`, which
/// can say yes to a permission) still ask.
///
/// A rule ending `:*` matches the command with any arguments or none, but
/// only as whole words: `crystal send:*` isn't `crystal send-keys`, and
/// `crystal task:*` isn't `crystal tasks cancel`.
fn crystal_commands(config: &Config) -> Vec<&'static str> {
    // Sessions: starting, driving and reading them, and what's on screen.
    let mut rules = vec![
        "Bash(crystal ls:*)",
        "Bash(crystal new:*)",
        "Bash(crystal send:*)",
        "Bash(crystal wait:*)",
        "Bash(crystal read:*)",
        "Bash(crystal result:*)",
        "Bash(crystal interrupt:*)",
        "Bash(crystal events:*)",
        "Bash(crystal rename:*)",
        "Bash(crystal report:*)",
        "Bash(crystal notify:*)",
        "Bash(crystal layout:*)",
        "Bash(crystal pane split:*)",
        "Bash(crystal pane close:*)",
    ];
    if tasks::enabled(config) {
        rules.extend([
            "Bash(crystal done:*)",
            "Bash(crystal task:*)",
            "Bash(crystal tasks)",
            "Bash(crystal tasks --all)",
            "Bash(crystal tasks show:*)",
            "Bash(crystal tasks log:*)",
            "Bash(crystal tasks new:*)",
            "Bash(crystal tasks start:*)",
        ]);
    }
    if flows::enabled(config) {
        rules.extend([
            "Bash(crystal flow)",
            "Bash(crystal flow --json)",
            "Bash(crystal flow run:*)",
            "Bash(crystal flow wait:*)",
            "Bash(crystal flow show:*)",
            "Bash(crystal flow defs:*)",
            "Bash(crystal flow retry:*)",
        ]);
    }
    if backlog::enabled(config) {
        rules.extend([
            "Bash(crystal backlog add:*)",
            "Bash(crystal backlog)",
            "Bash(crystal backlog --all)",
            "Bash(crystal backlog export)",
            "Bash(crystal backlog done:*)",
            "Bash(crystal backlog reopen:*)",
            "Bash(crystal backlog start:*)",
        ]);
    }
    if handoff::enabled(config) {
        rules.push("Bash(crystal handoff:*)");
    }
    if memory::enabled(config) {
        rules.extend([
            "Bash(crystal remember:*)",
            "Bash(crystal memory)",
            "Bash(crystal memory search:*)",
            "Bash(crystal memory show:*)",
        ]);
    }
    rules
}

/// The arguments each of a task's runs gives Claude: the task's own; in its
/// system prompt, what it's told of the pull request and the issue it's
/// about, the notes its worktree's sessions left and, with memory on, what
/// its project remembers that has to do with its prompt; with memory on,
/// crystal's MCP server, to search the rest; and the crystal commands it's
/// told to run and that server's tools allowed.
fn task_args(
    socket: &Path,
    cwd: &Path,
    spec: &protocol::TaskSpec,
    brief: &TaskBrief,
) -> Vec<String> {
    let config = settings();
    let memory_on = memory::enabled(&config);
    let remembered = memory_on
        .then(|| launch_memory(socket, cwd, &spec.prompt, memory::Reader::Task))
        .flatten();
    // Its runs close it, so it's told nothing of closing it: only of the
    // pull request and the issue it's about.
    let about = tasks::forge_notes(brief, cwd);
    let handoff = handoff_note(cwd, &[about.as_deref(), remembered.as_deref()]);
    let mut args = agents::with_instructions(&spec.args, &notes(about, None, handoff, remembered));
    let mut tools = crystal_commands(&config);
    if memory_on && let Ok(crystal) = std::env::current_exe() {
        let server = mcp::config(&crystal, socket, cwd);
        args = agents::with_value(&args, &["--mcp-config"], &server);
        tools.extend(mcp::TOOLS);
    }
    if tools.is_empty() {
        return args;
    }
    agents::with_value(
        &args,
        &["--allowedTools", "--allowed-tools"],
        &tools.join(","),
    )
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
    start_task_as(
        new_id(),
        sessions,
        socket,
        spending,
        task,
        conversation,
        run_prompt,
    )
}

/// Makes a task under the id `id`, as [`start_task`] does.
fn start_task_as(
    id: String,
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
        brief,
    } = task;
    check_dir(&cwd)?;
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

    let mut env = env::for_session(&env, &name, &id, socket);
    // The task follows its Claude's own events: the hooks the user
    // installed stay quiet.
    env.insert(agents::HOOKED.into(), "claude".into());
    let prompt = spec.prompt.clone();
    let args = task_args(socket, &cwd, &spec, &brief);
    // Its acceptance criteria go under its prompt.
    let first_prompt = tasks::with_criteria(&prompt, &brief.accept);
    let spending = spending.clone();
    keep_scrollback();
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
        session.give_task(new_task_info(prompt, true, backlog, brief));
    }
    if run_prompt {
        session.prompt(&first_prompt)?;
    } else {
        session.came_back();
    }
    sessions.push(session);
    Ok(name)
}

/// A task just made, open, and numbered by [`Daemon::number_tasks`].
fn new_task_info(
    goal: String,
    background: bool,
    backlog: Option<u64>,
    brief: TaskBrief,
) -> TaskInfo {
    TaskInfo {
        id: None,
        goal,
        background,
        backlog,
        waiting: false,
        created: now_seconds(),
        outcome: None,
        brief,
    }
}

/// The user's settings, read again each time so that a change counts at
/// once. A file that can't be read leaves the defaults.
fn settings() -> Config {
    Config::load().unwrap_or_default()
}

/// Has the screens made from now on keep the history the settings say.
fn keep_scrollback() {
    vt::set_history_lines(settings().scrollback_lines);
}

/// Now, in seconds since the Unix epoch.
fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Refuses a name a session can't have: an empty one, one with spaces or
/// a character a terminal would take as an order (it's printed as it is),
/// or one another session has.
fn check_name(name: &str, taken: impl Fn(&str) -> bool) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.contains(char::is_whitespace),
        "a session name can't be empty or contain spaces"
    );
    ensure!(
        !name.contains(printable::is_unprintable),
        "a session name can't contain control characters"
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

/// The session a hook's report is about, by its id: `id` is the one the
/// hook's environment names. Claude Code runs its hooks itself, so that's
/// the one. Codex runs them in a server its sessions share, started from
/// whichever ran Codex first, so its environment can name another session:
/// a Codex report goes to the session in the conversation it names; or to
/// the one its environment names, if Codex may be running there in no
/// conversation yet; or to the only session that fits, of those Codex may
/// be running in with no conversation yet, by the report's directory when
/// there are several, or of all those Codex may be running in. `None` when
/// none of them is sure: crystal reads Codex's screen all the same.
fn reporting_session(
    sessions: &[Session],
    agent: &str,
    id: &str,
    conversation: Option<&Conversation>,
    cwd: Option<&Path>,
) -> Option<String> {
    if agent != "codex" {
        return Some(id.to_string());
    }
    let candidates: Vec<Candidate> = sessions
        .iter()
        .map(|session| Candidate {
            id: &session.id,
            conversation: session.conversation_id(),
            may_run: session.is_running() && session.may_run(agent),
            cwd: session.cwd(),
        })
        .collect();
    let named = conversation.map(|conversation| conversation.id.as_str());
    pick_reporting(&candidates, id, named, cwd).map(String::from)
}

/// A session as [`pick_reporting`] sees it.
struct Candidate<'a> {
    id: &'a str,
    conversation: Option<&'a str>,
    /// Whether the agent can be running in it.
    may_run: bool,
    cwd: &'a Path,
}

/// The id of the session a Codex report is about, of `sessions`, as
/// [`reporting_session`] picks it: `id` is the one the hook's environment
/// names, `conversation` the one the report names.
fn pick_reporting<'a>(
    sessions: &[Candidate<'a>],
    id: &str,
    conversation: Option<&str>,
    cwd: Option<&Path>,
) -> Option<&'a str> {
    if let Some(found) = sessions
        .iter()
        .find(|session| conversation.is_some() && session.conversation == conversation)
    {
        return Some(found.id);
    }
    let only = |found: Vec<&Candidate<'a>>| match found[..] {
        [session] => Some(session.id),
        _ => None,
    };
    let fresh: Vec<&Candidate> = (sessions.iter())
        .filter(|session| session.may_run && session.conversation.is_none())
        .collect();
    if let Some(named) = fresh.iter().find(|session| session.id == id) {
        return Some(named.id);
    }
    let in_cwd = (fresh.iter().copied())
        .filter(|session| cwd == Some(session.cwd))
        .collect();
    only(in_cwd)
        .or_else(|| only(fresh))
        .or_else(|| only(sessions.iter().filter(|session| session.may_run).collect()))
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
/// ([`socket::crystal_for`]), makes the two match; one older than the
/// daemon, say a TUI left open as crystal was upgraded, is the one to start
/// again.
fn version_mismatch(daemon: &str, client: Option<&str>, crystal: &str) -> String {
    match client {
        Some(client) if older(client, daemon) => format!(
            "this is crystal {client}, but the daemon is crystal {daemon}, which is newer: \
             quit and run {crystal} again to use it"
        ),
        client => {
            let client = match client {
                Some(version) => format!("crystal {version}"),
                None => "an older crystal".to_string(),
            };
            format!(
                "this is {client}, but the daemon is crystal {daemon}: \
                 run `{crystal} restart-server` to restart the daemon on this crystal"
            )
        }
    }
}

/// Whether the version `a` comes before `b`, number by number. Neither,
/// when either isn't numbers.
fn older(a: &str, b: &str) -> bool {
    let numbers = |version: &str| {
        version
            .split('.')
            .map(|number| number.parse::<u64>().ok())
            .collect::<Option<Vec<u64>>>()
    };
    match (numbers(a), numbers(b)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
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
/// Refuses to start a session in a directory that isn't there, which the
/// terminal would start it somewhere else for: in the home directory.
fn check_dir(cwd: &Path) -> Result<()> {
    let dir = crate::shell::home_relative(cwd);
    ensure!(cwd.is_dir(), "its directory, {dir}, isn't there");
    Ok(())
}

/// Whether `saved` starts an agent again, which a restart spaces out: one
/// with a conversation to pick up or a command to resume it, or whose
/// program is an agent's. A task comes back at rest, with nothing to run.
fn starts_an_agent(saved: &SavedSession) -> bool {
    if saved.task.is_some() {
        return false;
    }
    let agent = matches!(front::of_command(&saved.command), Some(Front::Agent { .. }));
    agent || saved.conversation.is_some() || saved.resume.is_some()
}

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
        .map(|name| printable::line(&name.to_string_lossy()).replace(char::is_whitespace, "-"))
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
    fn claude_may_run_crystal_s_own_commands_but_not_those_that_remove_or_answer() {
        let mut config = Config::default();
        let rules = crystal_commands(&config);
        for allowed in [
            "Bash(crystal new:*)",
            "Bash(crystal send:*)",
            "Bash(crystal wait:*)",
            "Bash(crystal read:*)",
            "Bash(crystal task:*)",
            "Bash(crystal flow run:*)",
            "Bash(crystal backlog done:*)",
            "Bash(crystal memory search:*)",
        ] {
            assert!(rules.contains(&allowed), "{allowed}: {rules:?}");
        }
        // Each of these still asks the user: no rule is a prefix of it.
        for asks in [
            "crystal kill",
            "crystal worktree rm",
            "crystal send-keys",
            "crystal answer",
            "crystal tasks cancel",
            "crystal flow cancel",
            "crystal flow approve",
            "crystal flow back",
            "crystal backlog rm",
            "crystal memory rm",
            "crystal memory promote",
            "crystal kill-server",
        ] {
            let covers = |rule: &&str| {
                let rule = rule.trim_start_matches("Bash(").trim_end_matches(')');
                match rule.strip_suffix(":*") {
                    Some(prefix) => asks == prefix || asks.starts_with(&format!("{prefix} ")),
                    None => asks == rule,
                }
            };
            assert!(!rules.iter().any(covers), "{asks}");
        }
        for plugin in ["tasks", "flows", "backlog", "handoff", "memory"] {
            config.plugins.insert(plugin.into(), false);
        }
        let rules = crystal_commands(&config);
        assert!(rules.contains(&"Bash(crystal send:*)"));
        assert!(!rules.iter().any(|rule| rule.contains("done")
            || rule.contains("flow")
            || rule.contains("backlog")
            || rule.contains("handoff")
            || rule.contains("memory")
            || rule.contains("task")));
    }

    fn candidate<'a>(id: &'a str, conversation: Option<&'a str>, cwd: &'a str) -> Candidate<'a> {
        Candidate {
            id,
            conversation,
            may_run: true,
            cwd: Path::new(cwd),
        }
    }

    #[test]
    fn a_codex_report_goes_to_the_session_in_the_conversation_it_names() {
        let sessions = [
            candidate("a", Some("conv-a"), "/app"),
            candidate("b", Some("conv-b"), "/app"),
        ];
        // Whatever session the shared server's environment names.
        let pick = |conversation| pick_reporting(&sessions, "a", conversation, None);
        assert_eq!(pick(Some("conv-b")), Some("b"));
        assert_eq!(pick(Some("conv-a")), Some("a"));
        // A conversation nobody's in, with two sessions it could be from.
        assert_eq!(pick(Some("conv-c")), None);
    }

    #[test]
    fn a_new_codex_conversation_goes_where_it_can_only_be() {
        let sessions = [
            candidate("old", Some("conv-1"), "/app"),
            candidate("named", None, "/app"),
            candidate("here", None, "/lib"),
            Candidate {
                may_run: false,
                ..candidate("shell", None, "/lib")
            },
        ];
        let pick = |id, cwd: &str| pick_reporting(&sessions, id, Some("new"), Some(Path::new(cwd)));
        // The session the environment names, if Codex can be new there.
        assert_eq!(pick("named", "/lib"), Some("named"));
        // Or else the one in the report's directory.
        assert_eq!(pick("old", "/lib"), Some("here"));
        assert_eq!(pick("shell", "/elsewhere"), None, "two could be it");
        // The only Codex session there is takes a conversation of its own
        // started from inside it.
        let one = [candidate("only", Some("conv-1"), "/app")];
        assert_eq!(
            pick_reporting(&one, "gone", Some("new"), None),
            Some("only")
        );
    }

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
    fn a_crystal_older_than_the_daemon_is_told_to_start_again() {
        assert_eq!(
            version_mismatch("0.4.0", Some("0.3.9"), "crystal --server work"),
            "this is crystal 0.3.9, but the daemon is crystal 0.4.0, which is newer: \
             quit and run crystal --server work again to use it"
        );
        assert!(older("0.9.0", "0.10.0"));
        assert!(!older("0.10.0", "0.9.0"));
        assert!(!older("dev", "0.1.0"));
    }

    #[test]
    fn crystal_s_notes_say_where_they_come_from_then_the_task_then_the_memory() {
        let notes = notes(
            Some("about the task".into()),
            Some("in parallel".into()),
            Some("handed off".into()),
            Some("remembered".into()),
        );
        assert_eq!(
            notes,
            [
                agents::ABOUT_CRYSTAL,
                "about the task",
                "in parallel",
                "handed off",
                "remembered"
            ]
        );
    }

    #[test]
    fn with_nothing_to_say_there_are_no_notes_at_all() {
        assert!(notes(None, None, None, None).is_empty());
        assert_eq!(notes(None, None, None, Some("remembered".into())).len(), 2);
        assert_eq!(notes(None, None, Some("handed off".into()), None).len(), 2);
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
        assert_eq!(unique_name("/bin/\x1b]0;t\x07", |_| false), "]0;t");
    }

    #[test]
    fn a_name_has_to_be_free_and_one_word() {
        let taken = |name: &str| name == "claude";
        assert!(check_name("reviewer", taken).is_ok());
        assert!(check_name("claude", taken).is_err());
        assert!(check_name("", taken).is_err());
        assert!(check_name("two words", taken).is_err());
        for hostile in ["a\x1b]0;t\x07", "b\u{9b}2J", "c\u{202e}d", "e\x7f"] {
            assert!(check_name(hostile, taken).is_err(), "{hostile:?}");
        }
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
