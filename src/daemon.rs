//! The background process that owns every session. It outlives the
//! terminal it was started from, so sessions keep running when the
//! client goes away.

mod moving;
mod removal;
mod spare;

use crate::agent_rules;
use crate::agents;
use crate::artifacts;
use crate::backlog;
use crate::catalog;
use crate::codex;
use crate::config::{self, Config, MemorySettings};
use crate::db;
use crate::db::Db;
use crate::distill::{self, Job};
use crate::drive;
use crate::embed;
use crate::env;
use crate::event_log::{self, Bus, Subscription};
use crate::events::{DistillAbout, Event, Filter, Kind, Since};
use crate::flow_run::{self, Ended, FlowRun, Next, Place, RunState, StepState};
use crate::flows;
use crate::front;
use crate::git::{self, Checkout};
use crate::handoff;
use crate::handover::{self, Gate, Ticket};
use crate::layout::{self, Layout, Order};
use crate::layout_relay::{NoTui, Relay};
use crate::mcp;
use crate::memory;
use crate::messages::{self, Sender};
use crate::names;
use crate::notify::{self, Notice};
use crate::output::errln;
use crate::plugin_hooks;
use crate::printable;
use crate::project;
use crate::protocol::{
    self, AgentEvent, ArchivedSession, Artifact, ArtifactKind, Backlog, Conversation, Frame, Front,
    Metadata, NewSession, NewTask, PendingTask, Request, Response, SessionInfo, State, TaskBrief,
    TaskInfo, TaskOutcome, TaskRecord, TaskSpec, TaskStart, TaskState, TaskView, Worktree,
};
use crate::report;
use crate::resources;
use crate::session::{Change, STOP_GRACE, Session, Term, signal_group};
use crate::skill;
use crate::socket;
use crate::spending::Spending;
use crate::state::{self, SavedSession};
use crate::task;
use crate::tasks;
use crate::typing;
use crate::vt;
use crate::worktree_hooks;
use anyhow::{Context, Result, anyhow, bail, ensure};
use moving::{Move, written_down};
use regex::Regex;
use removal::Removal;
use spare::Spare;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::io::{BufReader, ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{fs, process, thread};

/// How often the daemon reads every session's screen for what its agent is
/// doing, tells the user about the sessions that need them, and writes
/// down the sessions that are running.
const KEEP_UP_EVERY: Duration = Duration::from_millis(250);

/// How often the keep-up loop looks at what terminals show, to keep it
/// while `[sessions] restore_screens` is on: the settings are read each
/// time. See [`Daemon::keep_screens`].
const SCREENS_CHECK_EVERY: Duration = Duration::from_secs(1);

/// How often what a terminal shows is kept again while it goes on changing.
const KEEP_SCREEN_EVERY: Duration = Duration::from_secs(15);

/// How often the keep-up loop looks for agents that have sat idle for too
/// long, and for another amount of history for the sessions to keep: the
/// settings are read each time. `CRYSTAL_IDLE_CHECK_MS` sets it otherwise,
/// for tests.
fn idle_check_every() -> Duration {
    std::env::var("CRYSTAL_IDLE_CHECK_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .map_or(Duration::from_secs(15), Duration::from_millis)
}

/// How long `crystal send` waits for an agent crystal stopped idle, started
/// again to take what's sent, to be back at its prompt.
const WAKE_TO_SEND: Duration = Duration::from_secs(60);

/// How often a session being typed into, or started again to be, is
/// looked at: see [`typing`].
const TYPING_LOOK_EVERY: Duration = Duration::from_millis(50);

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

/// How often the daemon looks for entries of memory gone stale, besides
/// each time a task closes.
const STALE_SWEEP_EVERY: Duration = Duration::from_secs(60 * 60);

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
    worktree_hooks::follow(&events, socket);
    // Wikis kept up to date as their default branches move.
    crate::wiki::auto::follow(socket);
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
    let (sweep, sweeps) = mpsc::sync_channel(1);
    thread::spawn({
        let events = events.clone();
        let socket = socket.to_path_buf();
        move || tell_stale(&socket, &events, &sweeps)
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
        removals: Mutex::default(),
        moves: Mutex::default(),
        spare: Mutex::default(),
        sweep,
        earlier: Mutex::default(),
    });
    // A daemon starts again after every upgrade, or is handed over to the
    // new crystal, so this is where the skill an earlier crystal installed
    // learns this one's commands: before the saved sessions start, so that
    // Claude Code in them reads the new one.
    match skill::refresh() {
        Ok(Some(path)) => errln!("crystal daemon: updated the skill in {}", path.display()),
        Ok(None) => {}
        Err(err) => errln!("crystal daemon: couldn't update the skill: {err:#}"),
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
                errln!(
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
    errln!("crystal daemon: couldn't take over: {err:#}");
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
    /// The worktrees being removed, with who's waiting to hear each is
    /// done. Taken after `sessions` and `flows`, never before them.
    removals: Mutex<Vec<Removal>>,
    /// The sessions on their way into other worktrees. Taken after
    /// `sessions`, never before it.
    moves: Mutex<Vec<Move>>,
    /// The agent kept warm for a new session to take over, while
    /// `[sessions] warm_agent` is on: see [`spare`]. Taken after
    /// `sessions`, never before it.
    spare: Mutex<Option<Spare>>,
    /// Asks for a look for entries of memory gone stale: see [`tell_stale`].
    sweep: SyncSender<()>,
    /// The CPU time each process had at the looks at what crystal takes,
    /// for the next look to count from.
    earlier: Mutex<resources::Earlier>,
}

/// The projects the sessions run in that are on the list already, as
/// this daemon has put them there, and each listed project's repository,
/// found once: reading which branch it's on is cheap after that.
#[derive(Default)]
struct KnownProjects {
    listed: HashSet<PathBuf>,
    checkouts: HashMap<PathBuf, Option<Checkout>>,
    /// The tokens `crystal project report` put on each project's rows,
    /// listed or not.
    shown: HashMap<PathBuf, report::Shown>,
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
                errln!("crystal daemon: {err:#}");
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
                program,
            } => {
                drop(ticket);
                return match self.find(name.as_deref(), !program) {
                    Ok(found) => attach(&conn, input, found, (rows, cols), history, program),
                    Err(err) => Ok(protocol::send(&conn, &Response::from(err))?),
                };
            }
            Request::Subscribe { filter, since } => {
                drop(ticket);
                return self.stream_events(&conn, filter, since);
            }
            Request::TakeLayoutOrders { used } => {
                drop(ticket);
                return self
                    .layout
                    .serve(&conn, input, used, |event| self.events.emit(event));
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
            Request::RemoveWorktree { path, force } => {
                return self.remove_worktree(&conn, ticket, path, force);
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
            errln!("crystal daemon: couldn't read the sessions to start again: {err:#}");
            Vec::new()
        });
        sessions.extend(
            saved
                .into_iter()
                .map(|saved| Session::put_back(new_id(), saved)),
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
        let before = self.screen_before(&saved);
        // It makes way, so that its name is free for the session started.
        let mut waiting = sessions.remove(index);
        let id = waiting.id.clone();
        match self.start_saved(sessions, saved.clone(), env, Some(id), before) {
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
                let started = &sessions[index];
                self.events
                    .emit(Event::started(&started.info(), started.resumed()));
                Ok(name)
            }
            Err(err) => {
                let why = format!("{err:#}");
                errln!("crystal daemon: couldn't start {} again: {why}", saved.name);
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

    /// What the terminal of the session written down as `saved` showed
    /// before the restart, to show again above its program, while `[sessions]
    /// restore_screens` is on: see [`Daemon::keep_screens`].
    fn screen_before(&self, saved: &SavedSession) -> Option<vt::Saved> {
        if !shows_again(saved) || !settings().sessions.restore_screens {
            return None;
        }
        let screen = self.db.lock().unwrap().screen(&saved.name);
        screen.unwrap_or_else(|err| {
            errln!(
                "crystal daemon: couldn't read what {} showed: {err:#}",
                saved.name
            );
            None
        })
    }

    /// Starts a session again from what was written down of it, from the
    /// environment `env`, at the end of `sessions`, under the id `id` or a
    /// new one, and gives back its name: an agent in its conversation, a
    /// task at rest, any other program from the start, below `before`, what
    /// its terminal showed before (see [`start_as`]). It comes back with its
    /// task as it was, closed or not. One written down on its way into
    /// another worktree, which it starts in, is told it has moved: an agent
    /// as it starts, a task with a follow-up, which carries it on there.
    fn start_saved(
        &self,
        sessions: &mut Vec<Session>,
        saved: SavedSession,
        env: BTreeMap<String, String>,
        id: Option<String>,
        before: Option<vt::Saved>,
    ) -> Result<String> {
        let id = id.unwrap_or_else(new_id);
        let moved = saved.moved.as_ref().map(moving::notice);
        let goal = saved.goal.clone();
        let backlog = goal.as_ref().and_then(|goal| goal.backlog);
        let brief = brief_of(&saved);
        let name_given = saved.name_given;
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
                    agent_names: false,
                };
                start_as(
                    id,
                    sessions,
                    &self.socket,
                    new,
                    saved.conversation,
                    saved.resume,
                    moved.as_deref(),
                    before,
                )?
            }
        };
        let session = sessions.last_mut().expect("start added a session");
        if let Some(goal) = goal {
            session.give_task(goal);
        }
        // A name the user or a script gave stays theirs.
        if name_given {
            session.keep_given_name();
        }
        if let Some(notice) = moved.filter(|_| session.is_task())
            && let Err(err) = session.prompt(&notice)
        {
            errln!("crystal daemon: couldn't tell {name} it has moved: {err:#}");
        }
        Ok(name)
    }

    /// Stops the session called `name` and keeps it in the archive, out of
    /// the list. Written down before it stops: a session that couldn't be
    /// kept isn't stopped. Its open task is cancelled, as a kill does, but
    /// it's kept open, to be open again when it starts again. Then the
    /// distiller reads what it did, as it would have once its task closed.
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
        if let Some(cancelled) = &cancelled {
            self.write_down_closed(&session, cancelled);
        }
        self.distill_archived(&session, cancelled.as_ref());
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
        let session = archived.session.clone();
        let started = match self.start_saved(&mut sessions, session, env, None, None) {
            Ok(started) => started,
            Err(err) => {
                db.archive(&archived)?;
                return Err(err);
            }
        };
        drop(db);
        let response = self.started(&mut sessions, started.clone(), Kind::TaskOpened);
        if let Some(session) = sessions.iter().find(|session| session.name == started) {
            self.events.emit(Event::about_session(
                Kind::SessionUnarchived,
                &session.info(),
            ));
        }
        Ok(response)
    }

    /// Again and again: reads every session's screen for what its agent is
    /// doing, tells what has changed, and writes down the running sessions
    /// when they've changed, and what their terminals show.
    fn keep_up(&self) {
        let mut last_saved: Vec<SavedSession> = Vec::new();
        let mut kept_screens = KeptScreens::default();
        let mut screens_checked = Instant::now();
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
            let mut retitled = Vec::new();
            for session in sessions.iter_mut() {
                session.find_conversation(&claimed, &looking);
                session.check_front();
                session.check();
                session.check_model();
                if let Some(title) = session.check_title() {
                    retitled.push((session.id.clone(), title));
                }
                self.tell_changes(session);
                for closed in session.take_closed() {
                    self.write_down_closed(session, &closed);
                }
            }
            for (id, title) in retitled {
                self.follow_claude_title(&mut sessions, &id, &title);
            }
            // Before an ended session is told of: one moving into another
            // worktree starts again there instead.
            self.carry_out_moves(&mut sessions);
            // Before telling the user anything: a step the flow goes on
            // from needs nobody, and a gate needs them.
            self.follow_flows(&mut sessions);
            if idle_checked.elapsed() >= idle_check_every {
                idle_checked = Instant::now();
                let settings = settings();
                stop_idle_sessions(&mut sessions, &settings);
                follow_scrollback(&sessions, &settings);
                self.keep_spare(&settings);
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
            // One running again under its id, moved into another worktree,
            // is told of again when it ends.
            told_ended.retain(|id| {
                (sessions.iter()).any(|session| &session.id == id && !session.is_running())
            });
            self.list_projects_of(&sessions);
            // Written while the list is still locked, so that an older list
            // can never be written after a shutdown has emptied it; and with
            // the flow runs, in one go, so that a crash never leaves a step's
            // session written down without its run.
            let saved = written_down(&sessions, &self.moves.lock().unwrap());
            let runs = self.flows.lock().unwrap().clone();
            if saved != last_saved || runs != last_runs {
                match self
                    .db
                    .lock()
                    .unwrap()
                    .save_sessions_and_runs(&saved, &runs)
                {
                    Ok(()) => {
                        last_saved = saved;
                        last_runs = runs;
                    }
                    Err(err) => {
                        errln!("crystal daemon: couldn't save the sessions and flow runs: {err:#}")
                    }
                }
            }
            if screens_checked.elapsed() >= SCREENS_CHECK_EVERY {
                screens_checked = Instant::now();
                self.keep_screens(&sessions, &mut kept_screens);
            }
        }
    }

    /// Keeps what each terminal shows in the database, while `[sessions]
    /// restore_screens` is on, to show again above its program should a
    /// crash or a reboot have it start again: a screen not kept yet as soon
    /// as there's something on it, then again once it has changed, at most
    /// every [`KEEP_SCREEN_EVERY`]. A task's isn't kept, nor that of an
    /// agent a restart would pick up in its conversation, which shows its
    /// own; one yet to start again keeps what was kept. With the setting
    /// off, nothing is kept, and what was is forgotten. Called with the
    /// sessions locked, so nothing is kept once a shutdown has emptied them.
    fn keep_screens(&self, sessions: &[Session], kept: &mut KeptScreens) {
        if !settings().sessions.restore_screens {
            if kept.maybe_some {
                match self.db.lock().unwrap().forget_screens() {
                    Ok(()) => *kept = KeptScreens::nothing(),
                    Err(err) => errln!("crystal daemon: couldn't forget the screens: {err:#}"),
                }
            }
            return;
        }
        kept.maybe_some = true;
        kept.sessions
            .retain(|id, _| sessions.iter().any(|session| &session.id == id));
        let db = self.db.lock().unwrap();
        for session in sessions.iter().filter(|session| session.is_running()) {
            let Some(saved) = session.saved() else {
                continue;
            };
            let was = kept.sessions.get(&session.id);
            let renamed = was.is_some_and(|was| was.name != session.name);
            if !shows_again(&saved) {
                if renamed || was.is_none_or(|was| was.kept.is_some()) {
                    if let Err(err) = db.forget_screen(&session.name) {
                        errln!("crystal daemon: couldn't forget a screen: {err:#}");
                        continue;
                    }
                    let forgotten = KeptScreen {
                        name: session.name.clone(),
                        kept: None,
                    };
                    kept.sessions.insert(session.id.clone(), forgotten);
                }
                continue;
            }
            let term = session.term();
            let written = term.main_written();
            let due = match was.and_then(|was| was.kept.filter(|_| !renamed)) {
                Some((then, at)) => then != written && at.elapsed() >= KEEP_SCREEN_EVERY,
                None => true,
            };
            if !due {
                continue;
            }
            let screen = term.kept_screen();
            // Looked at again next time.
            if screen.output.is_empty() {
                continue;
            }
            match db.keep_screen(&session.name, &screen) {
                Ok(()) => {
                    let now = KeptScreen {
                        name: session.name.clone(),
                        kept: Some((written, Instant::now())),
                    };
                    kept.sessions.insert(session.id.clone(), now);
                }
                Err(err) => errln!(
                    "crystal daemon: couldn't keep what {} shows: {err:#}",
                    session.name
                ),
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
            let listed = self.db.lock().unwrap().list_project(project, true);
            match listed {
                Ok(added) => {
                    known.listed.insert(project.to_path_buf());
                    if added {
                        let event = Event::project_listed(true, project.to_path_buf());
                        self.events.emit(event);
                    }
                }
                Err(err) => errln!(
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
        let changed = self.db.lock().unwrap().list_project(&project, listed)?;
        if listed {
            known.checkouts.insert(project.clone(), Some(checkout));
        } else {
            known.listed.remove(&project);
            known.checkouts.remove(&project);
        }
        if changed {
            self.events.emit(Event::project_listed(listed, project));
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
            errln!("crystal daemon: couldn't read the flow runs: {err:#}");
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
        errln!("crystal daemon: handing over to {}", exe.display());
        handover::begin();
        let deadline = Instant::now() + HANDOVER_GRACE;
        let waiting = self.gate.close(&self.socket, deadline);
        let Err(err) = self.exec_handed_over(&waiting, exe, deadline);
        // The sessions can't carry on: the daemon stops as a shutdown that
        // keeps them does, and the client starts the next, which starts them
        // again. Its socket goes first, so the client finds it gone.
        errln!("crystal daemon: couldn't hand over, so it stops: {err:#}");
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
        // The agent kept warm isn't handed over: a TUI asks for another.
        self.drop_spare();
        let mut sessions = self.sessions.lock().unwrap();
        let flows = self.flows.lock().unwrap();
        // Held until the exec, so each removal's git is handed over either
        // running, for the next crystal to wait for, or reaped.
        let removals = self.removals.lock().unwrap();
        let moves = self.moves.lock().unwrap();
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
        let saved = written_down(&sessions, &moves);
        self.db
            .lock()
            .unwrap()
            .save_sessions_and_runs(&saved, &flows)?;
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
            removals: removals
                .iter()
                .map(Removal::hand_over)
                .collect::<std::io::Result<_>>()?,
            moves: moves.iter().map(Move::hand_over).collect(),
            project_tokens: self.projects.lock().unwrap().shown.clone(),
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
            removals,
            moves,
            project_tokens,
            ..
        } = handed;
        self.projects.lock().unwrap().shown = project_tokens;
        let mut sessions = self.sessions.lock().unwrap();
        let mut again = Vec::new();
        for handed in handed_sessions {
            let name = handed.name().to_string();
            let saved = handed.saved();
            let processes = handed.processes();
            match Session::adopt(handed, &self.spending) {
                Ok(session) => sessions.push(session),
                Err(err) => {
                    errln!("crystal daemon: couldn't carry {name} on: {err:#}");
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
        self.carry_on_removals(removals);
        self.carry_on_moves(moves);
        errln!("crystal daemon: took over from crystal {from}: {carried} sessions carried on");
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
                let text = format!("{} failed at {}", run.name, run.step_name(step));
                let notice = Notice::of_crystal(text, session);
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
        let accept = run.criteria(step)?;
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
                agent_names: false,
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
        // rather than the whole of its prompt, with the step's acceptance
        // criteria, which its prompt has under it already.
        let session = sessions.last_mut().expect("it was just started");
        if session.task_record().is_some() {
            let goal = format!("{} {}: {}", run.name, run.step_name(step), run.goal);
            let brief = TaskBrief {
                accept,
                ..TaskBrief::default()
            };
            session.give_task(new_task_info(goal, !terminal, None, brief));
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
    /// asks for it, on a new branch with a made-up name, as the new-session
    /// panel's are.
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
                let location = git::Location {
                    path: None,
                    directory: settings().worktrees.directory(),
                };
                let branch = names::random();
                let (worktree, branch) =
                    git::add_new_worktree(&run.cwd, &branch, &base, &location)?;
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
                Err(err) => errln!("crystal daemon: couldn't number a task: {err:#}"),
            }
        }
    }

    /// Writes a task that has just closed in `session` into its project's
    /// history, as [`Daemon::write_down`] does. Then the distiller reads
    /// what it did.
    fn write_down_closed(&self, session: &Session, task: &TaskRecord) {
        self.write_down(session.cwd(), Some(&session.info()), task);
        self.distill_later(session, task);
        // What the task changed may leave entries of memory stale. One
        // look asked for already will do.
        let _ = self.sweep.try_send(());
    }

    /// Writes a task that has just closed, which ran in `cwd`, in `session`
    /// if it had started, into its project's history, and tells of it.
    /// When it was done and was for a backlog item, ticks the item.
    fn write_down(&self, cwd: &Path, session: Option<&protocol::SessionInfo>, task: &TaskRecord) {
        let project = project::of(cwd).path;
        let ticked = {
            let mut db = self.db.lock().unwrap();
            if let Err(err) = db.record_task(&project, task) {
                errln!("crystal daemon: couldn't write down a closed task: {err:#}");
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
                        errln!("crystal daemon: couldn't tick #{number} on the backlog: {err:#}");
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
                Err(err) => errln!("crystal daemon: couldn't add to a handoff file: {err:#}"),
            }
        }
        let Some(id) = task.id else {
            return;
        };
        let dir = state::task_dir(&self.socket, id);
        match artifacts::keep_handoff(&dir, &handoff::path(worktree)) {
            Ok(Some(kept)) => self.record_kept(info, task, &[kept]),
            Ok(None) => {}
            Err(err) => errln!("crystal daemon: couldn't keep t{id}'s handoff file: {err:#}"),
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
                    errln!("crystal daemon: couldn't write down a file t{id} kept: {err:#}")
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
            errln!("crystal daemon: couldn't read the files t{id} kept: {err:#}");
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
        if read_as_it_closed(task) {
            self.distill_in_background(session, Some(task));
        }
    }

    /// Has the distiller read what `session` did as it's archived, when it
    /// wasn't read as its task closed: a session with no task, or whose
    /// task archiving it `cancelled`.
    fn distill_archived(&self, session: &Session, cancelled: Option<&TaskRecord>) {
        let task = cancelled.cloned().or_else(|| session.task_record());
        if !task.as_ref().is_some_and(read_as_it_closed) {
            self.distill_in_background(session, task.as_ref());
        }
    }

    /// Has the distiller read what `session` did, on `task` if it had one,
    /// on a thread of its own, when memory is on and the config says to,
    /// and the session left something it can read.
    fn distill_in_background(&self, session: &Session, task: Option<&TaskRecord>) {
        let config = settings();
        if !memory::enabled(&config) || !config.memory.distill {
            return;
        }
        // A handover underway would only stop it halfway.
        if handover::underway() {
            return;
        }
        let Some(job) = self.distill_job(session, task, config.memory) else {
            return;
        };
        let Some(reading) = Reading::start(&self.distilling, &session.id) else {
            return;
        };
        let events = self.events.clone();
        let info = session.info();
        thread::spawn(move || {
            let name = &job.session;
            let report = distill::run(&job);
            match &report {
                Ok(report) => {
                    errln!("crystal daemon: distilled {name}: {}", report.line());
                    for why in &report.rejected {
                        errln!("crystal daemon:   rejected {why}");
                    }
                    tell_distilled(&events, &job, report);
                }
                Err(err) => errln!("crystal daemon: couldn't distill {name}: {err:#}"),
            }
            events.emit(Event::distilled(&info, distill_about(&report)));
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

    /// How the models that search memory by meaning stand. Asked while
    /// the config says not to search with them, the daemon lets them go;
    /// while entries have no vector yet from what searches use, and it
    /// could give them one (the models here loaded, or Gemini with a key
    /// and not resting after a failure), it does, in the background, rather
    /// than at the next search.
    fn embedding_status(&self) -> Result<embed::Status> {
        let settings = settings().memory;
        embed::let_go_unless(&settings);
        let mut status = embed::status(&self.socket, &settings)?;
        let could = match status.gemini {
            Some(_) => embed::gemini_ready(&settings),
            None => embed::is_loaded(),
        };
        if could && status.embedded < status.entries {
            self.prepare_embeddings(false);
        }
        let preparing = self.preparing.lock().unwrap();
        status.preparing = preparing.doing.map(String::from);
        status.failed = preparing.failed.clone();
        Ok(status)
    }

    /// Gets what searches memory by meaning ready, on a thread of its own,
    /// unless that's being done already: with Gemini, gives every entry its
    /// vector from it first, which takes seconds; then downloads the models
    /// here if they aren't yet and `download` says to, which takes minutes,
    /// and, while the config still says to search by meaning, loads them,
    /// lets go of the vectors of models it doesn't search with or fall back
    /// on, and with no Gemini, gives every entry its vector from them.
    fn prepare_embeddings(&self, download: bool) {
        {
            let mut preparing = self.preparing.lock().unwrap();
            if preparing.doing.is_some() {
                return;
            }
            *preparing = Preparing {
                doing: Some("getting the models ready"),
                failed: None,
            };
        }
        let preparing = self.preparing.clone();
        let socket = self.socket.clone();
        thread::spawn(move || {
            let doing = |what| preparing.lock().unwrap().doing = Some(what);
            let prepared = (|| -> Result<()> {
                let settings = settings().memory;
                let mut store = memory::Store::open(&socket)?;
                let gemini = settings.embedder == config::Embedder::Gemini;
                if gemini && let Some(remote) = embed::shared(&settings) {
                    doing("embedding the entries with Gemini");
                    // A failure is said as it happens, and kept for the
                    // status: the models here are got ready all the same.
                    if let Ok(count @ 1..) = store.embed_missing(&*remote) {
                        errln!(
                            "crystal daemon: embedded {count} entries of memory with {}",
                            remote.model()
                        );
                    }
                }
                let downloaded = embed::models_dir().is_some_and(|dir| embed::is_downloaded(&dir));
                if !downloaded {
                    if !download {
                        return Ok(());
                    }
                    doing("downloading the models");
                    errln!("crystal daemon: downloading {}", embed::names());
                    embed::download(false)?;
                }
                doing("loading the models");
                let Some(models) = embed::shared(&settings) else {
                    return Ok(());
                };
                store.forget_vectors_but(&[embed::MODEL, &embed::gemini_model(&settings)])?;
                if !gemini {
                    doing("embedding the entries");
                    match store.embed_missing(&*models)? {
                        0 => {}
                        count => errln!("crystal daemon: embedded {count} entries of memory"),
                    }
                }
                Ok(())
            })();
            let mut preparing = preparing.lock().unwrap();
            preparing.doing = None;
            if let Err(err) = prepared {
                errln!("crystal daemon: couldn't get memory's models ready: {err:#}");
                preparing.failed = Some(format!("{err:#}"));
            }
        });
    }

    /// Runs the distiller over what the session called `name` did, now,
    /// and says what came of it.
    fn distill_now(&self, name: &str) -> Result<Response> {
        let config = settings();
        crate::plugins::ensure_enabled(&config, "memory")?;
        let (job, reading, info) = {
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
            (job, reading, session.info())
        };
        let report = distill::run(&job);
        drop(reading);
        if let Ok(report) = &report {
            tell_distilled(&self.events, &job, report);
        }
        self.events
            .emit(Event::distilled(&info, distill_about(&report)));
        Ok(Response::Distilled(report?))
    }

    /// The session called `name`, or the newest one, for a client about to
    /// show it. That counts as having seen it, when it's the user who `sees`
    /// it.
    fn find(&self, name: Option<&str>, sees: bool) -> Result<Found> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = match name {
            Some(name) => named(&mut sessions, name)?,
            None => sessions.last_mut().context("there are no sessions")?,
        };
        if sees {
            session.seen();
            self.tell_changes(session);
        }
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
        self.wake_to_send(name)?;
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
                Some(_) => {
                    let tidied = messages::tidy(text)?;
                    messages::check_says_something(&tidied)?;
                    tidied
                }
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
        let typed = self.type_in(&info.id, name, &text, enter);
        self.sent(typed, &info, sender.as_ref(), &text)
    }

    /// Types `text` into the session with id `id`, called `name`, and
    /// presses Enter after it with `enter`. An agent at its prompt is
    /// watched until it takes it, as a [`typing::Delivery`] says: Enter
    /// goes once the text shows, and again while the agent neither starts
    /// on it nor changes its screen. One that never takes it has stalled,
    /// which the sender is told rather than that it went.
    fn type_in(&self, id: &str, name: &str, text: &str, enter: bool) -> Result<()> {
        let term = self.running_term(name)?;
        let (at_prompt, turns) = self.look_at(id)?;
        let screen = term.shown();
        term.write(&typing::keystrokes(text, term.wants_bracketed_paste()))?;
        if !enter {
            return Ok(());
        }
        if !at_prompt {
            thread::sleep(typing::ENTER_PAUSE);
            term.write(typing::ENTER)?;
            return Ok(());
        }
        let mut delivery = typing::Delivery::typed(screen, turns, Instant::now());
        loop {
            thread::sleep(TYPING_LOOK_EVERY);
            let (_, turns) = self.look_at(id)?;
            match delivery.look(term.shown(), turns, Instant::now()) {
                typing::Step::Wait => {}
                typing::Step::Enter => term.write(typing::ENTER)?,
                typing::Step::Taken => return Ok(()),
                typing::Step::Stalled => {
                    let enters = delivery.enters();
                    bail!(
                        "agent_prompt_stalled: {name} didn't take what it was sent: Enter was \
                         pressed {enters} times, and it neither started on it nor changed its \
                         screen, so it may still be in its input: `crystal read {name}` before \
                         sending it again"
                    )
                }
            }
        }
    }

    /// Whether the session with id `id` has its agent at its prompt, and
    /// how many turns it has begun.
    fn look_at(&self, id: &str) -> Result<(bool, u64)> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = with_id(&mut sessions, id)?;
        ensure!(session.is_running(), "{} has ended", session.name);
        Ok((session.at_prompt(), session.turns_begun()))
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

    /// Starts again the session called `name` when crystal stopped it as
    /// it sat idle, and waits for it to be ready to be typed into, as
    /// [`typing::Waking`] tells: its agent back at its prompt and reading
    /// keys, or a terminal's shell back, its screen held still. What's sent
    /// to an agent left idle, like another agent's message, is taken as if
    /// it had never stopped. Nothing for any other session.
    fn wake_to_send(&self, name: &str) -> Result<()> {
        let deadline = Instant::now() + WAKE_TO_SEND;
        // Once started again: whether it's an agent, and how it's coming.
        let mut woken: Option<(bool, typing::Waking)> = None;
        loop {
            {
                let mut sessions = self.sessions.lock().unwrap();
                let session = named(&mut sessions, name)?;
                match (session.stopped_idle(), session.is_running(), &mut woken) {
                    (false, _, None) => return Ok(()),
                    // Stopped, and not gone yet.
                    (true, true, None) => {}
                    (true, false, None) => {
                        // In the environment it started with, or after a
                        // restart, which forgot it, the daemon's.
                        let env = match session.env() {
                            env if env.is_empty() => env::current(),
                            env => env.clone(),
                        };
                        let agent = session.resumes_agent();
                        drop(sessions);
                        self.respawn(name, env)?;
                        woken = Some((agent, typing::Waking::new(Instant::now())));
                        continue;
                    }
                    (_, false, Some(_)) => bail!("{name} has ended"),
                    (_, true, Some((agent, waking))) => {
                        let term = session.term();
                        // A shell takes what's typed whenever it comes.
                        let look = typing::Look {
                            at_prompt: !*agent || session.at_prompt(),
                            takes_keys: !*agent || term.takes_keys(),
                            screen: term.shown(),
                        };
                        if waking.ready(look, Instant::now()) {
                            return Ok(());
                        }
                    }
                }
            }
            ensure!(
                Instant::now() < deadline,
                "{name} was stopped as it sat idle, and isn't back at its prompt yet"
            );
            thread::sleep(TYPING_LOOK_EVERY);
        }
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
        session.named_from_prompt();
        self.tell_renamed(session, &old_name);
    }

    /// Names the session with id `id` in `title`'s words, which its agent
    /// picked as crystal asked it to, while crystal still names it itself.
    fn name_by_agent(&self, id: &str, title: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().unwrap();
        let index = sessions
            .iter()
            .position(|session| session.id == id)
            .context("this session has gone")?;
        let session = &sessions[index];
        ensure!(
            settings().name_by_agent && session.awaits_agent_name(),
            "{} keeps its name",
            session.name
        );
        let base = names::from_title(title).context("say in a few words what it's about")?;
        let taken = |name: &str| {
            let others = sessions.iter().enumerate().filter(|(at, _)| *at != index);
            others
                .map(|(_, session)| session)
                .any(|session| session.name == name)
        };
        let new_name = unique_name(&base, taken);
        let session = &mut sessions[index];
        session.keep_name();
        if session.name != new_name {
            let old_name = std::mem::replace(&mut session.name, new_name);
            self.tell_renamed(session, &old_name);
        }
        Ok(())
    }

    /// Names the session with id `id` after `title`, the name Claude Code
    /// was just given for its conversation, as `/rename` gives one, unless
    /// the user or a script named the session.
    fn follow_claude_title(&self, sessions: &mut [Session], id: &str, title: &str) {
        let Some(index) = sessions.iter().position(|session| session.id == id) else {
            return;
        };
        let Some(base) = names::from_title(title) else {
            return;
        };
        if sessions[index].name_given() || sessions[index].name == base {
            return;
        }
        let taken = |name: &str| {
            let others = sessions.iter().enumerate().filter(|(at, _)| *at != index);
            others
                .map(|(_, session)| session)
                .any(|session| session.name == name)
        };
        let new_name = unique_name(&base, taken);
        let session = &mut sessions[index];
        if session.name == new_name {
            return;
        }
        let old_name = std::mem::replace(&mut session.name, new_name);
        session.keep_name();
        self.tell_renamed(session, &old_name);
    }

    /// What the session `id`, or else called `name`, is told of its
    /// project's memory as its agent reads or edits `file`, the first time
    /// it does: the entries about it it hasn't been shown, as
    /// [`Response::Context`]. Nothing for a file it was told of already, or
    /// that's outside its project, or with memory or `recall_on_read` off.
    /// The sessions aren't held while the memory is looked through.
    fn recall(&self, name: &str, id: Option<&str>, file: &Path) -> Result<Response> {
        let config = settings();
        if !memory::enabled(&config) || !config.memory.recall_on_read {
            return Ok(Response::Done);
        }
        let (id, lookup) = {
            let mut sessions = self.sessions.lock().unwrap();
            let session = id_or_name(&mut sessions, id.map(String::from), Some(name.to_string()))?;
            let Some(lookup) = session.recall_lookup(file) else {
                return Ok(Response::Done);
            };
            (session.id.clone(), lookup)
        };
        let (lookup, asked) = lookup;
        let mut store = memory::Store::open(&self.socket)?;
        let found = store.about_file(&lookup.project, &lookup.top, &lookup.file, &asked)?;
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.iter_mut().find(|session| session.id == id) else {
            return Ok(Response::Done);
        };
        Ok(match session.recalled().tell(&lookup.file, &found) {
            Some(text) => Response::Context { text },
            None => Response::Done,
        })
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
            Request::Warm(warm) => self.warm(warm),
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
                wakeup,
                said,
                pending,
            } => {
                let agent = agent.unwrap_or_else(|| "claude".to_string());
                // The agent kept warm is in no list till it's taken over.
                if let Some(id) = &id
                    && self.spare_heard(id, event, conversation.clone(), model.as_deref())
                {
                    return Ok(Response::Done);
                }
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
                // Only Claude Code's prompt hook takes a name for its
                // conversation, or more for it to read.
                let can_retitle = agent == "claude" && prompt.is_some();
                if let Some(prompt) = &prompt {
                    self.name_from_prompt(&mut sessions, &id, prompt);
                }
                let moving = self.is_moving(&id);
                let session = with_id(&mut sessions, &id)?;
                // Before it's reminded of its task: whether it asks the user
                // anything, and what of its own is still to come, say whether
                // its turn is over.
                if event == AgentEvent::TurnEnded {
                    session.turn_ended_with(said.as_deref(), pending);
                }
                if let Some(prompt) = &prompt {
                    session.recalled().asked(prompt);
                }
                if let Some(conversation) = conversation {
                    session.set_hooked_conversation(&agent, conversation);
                }
                if let Some(model) = model {
                    session.heard_model(&model);
                }
                if let Some(wakeup) = wakeup {
                    session.scheduled(wakeup);
                }
                // A conversation just named is looked at now: what's
                // written to it from here on is news, its model and its
                // name.
                session.check_model();
                if let Some(title) = session.check_title() {
                    self.follow_claude_title(&mut sessions, &id, &title);
                }
                let session = with_id(&mut sessions, &id)?;
                // Reminded that its task is open, the agent carries on: its
                // turn hasn't ended, and it isn't done. One moving into
                // another worktree carries on there.
                if event == AgentEvent::TurnEnded
                    && can_remind
                    && tasks::enabled(&settings())
                    && !moving
                    && session.remind_of_task()
                {
                    if let Some(task) = session.task_record() {
                        let info = session.info();
                        self.events
                            .emit(Event::task(Kind::TaskReminded, &info, task));
                    }
                    return Ok(Response::Remind {
                        text: tasks::REMINDER.to_string(),
                    });
                }
                let retitle = can_retitle.then(|| session.title_to_give()).flatten();
                // Asked to name the session, the agent says better what it's
                // about than the prompt's first words.
                let ask_name = can_retitle
                    && settings().name_by_agent
                    && session.ask_agent_to_name(prompt.as_deref().unwrap_or_default());
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
                // A session the user renamed isn't asked about: its name
                // stays.
                match retitle {
                    Some(title) => Ok(Response::Retitle { title }),
                    None if ask_name => Ok(Response::Context {
                        text: names::ASK_AGENT.to_string(),
                    }),
                    None => Ok(Response::Done),
                }
            }
            Request::Recall { name, id, file } => self.recall(&name, id.as_deref(), &file),
            Request::ReportAgent {
                id,
                name,
                report,
                source,
                seq,
            } => {
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
                // One passed over is answered all the same, as herdr's
                // are: a hook that ran late has nothing to put right.
                session.take_report(report, source, seq)?;
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
            Request::ReportProject { project, metadata } => {
                let only_tokens = Metadata {
                    tokens: metadata.tokens.clone(),
                    ttl_secs: metadata.ttl_secs,
                    source: metadata.source.clone(),
                    seq: metadata.seq,
                    ..Metadata::default()
                };
                ensure!(
                    metadata == only_tokens,
                    "a project's report puts --token on its rows, and nothing else"
                );
                let mut projects = self.projects.lock().unwrap();
                let shown = projects.shown.entry(project).or_default();
                shown.take(&metadata, SystemTime::now())?;
                Ok(Response::Done)
            }
            Request::ProjectTokens => {
                let now = SystemTime::now();
                let projects = self.projects.lock().unwrap();
                let tokens = projects.shown.iter().map(|(project, shown)| {
                    let tokens = shown.tokens(now);
                    (project.clone(), tokens)
                });
                Ok(Response::ProjectTokens {
                    projects: tokens.filter(|(_, tokens)| !tokens.is_empty()).collect(),
                })
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
            Request::Notify {
                text,
                id,
                name,
                title,
                sound,
            } => {
                let mut sessions = self.sessions.lock().unwrap();
                let session = match (id, name) {
                    (Some(id), _) => Some(with_id(&mut sessions, &id)?.name.clone()),
                    (None, Some(name)) => Some(named(&mut sessions, &name)?.name.clone()),
                    (None, None) => None,
                };
                drop(sessions);
                let notice = Notice::told(title.as_deref(), &text, session, sound)
                    .context("say what to tell the user")?;
                notify::tell(notice, &self.socket);
                Ok(Response::Done)
            }
            Request::Projects => Ok(Response::Projects {
                projects: self.known_projects()?,
            }),
            Request::MoveSession { name, path } => self.move_session(&name, &path),
            Request::AddProject { dir } => self.list_project(&dir, true),
            Request::RemoveProject { dir } => self.list_project(&dir, false),
            Request::Removals => Ok(Response::Removals {
                worktrees: self.removing(),
            }),
            Request::Subscribe { .. }
            | Request::WaitOutput { .. }
            | Request::Handover { .. }
            | Request::TakeLayoutOrders { .. }
            | Request::RemoveWorktree { .. } => {
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
                session.renamed();
                if new_name != name {
                    self.tell_renamed(session, &name);
                }
                Ok(Response::Done)
            }
            Request::NameByAgent { id, title } => {
                self.name_by_agent(&id, &title)?;
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
                self.wake_to_send(&name)?;
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
            Request::ProcessInfo { name } => {
                let (pid, term) = {
                    let mut sessions = self.sessions.lock().unwrap();
                    let session = named(&mut sessions, &name)?;
                    ensure!(
                        !session.is_task(),
                        "{name} is a background task, which runs in no terminal"
                    );
                    ensure!(session.is_running(), "{name} has ended");
                    (session.info().pid, session.term())
                };
                let group = term.foreground_group();
                let foreground = group.map(front::foreground).unwrap_or_default();
                Ok(Response::Processes(protocol::Processes {
                    pid,
                    group,
                    foreground,
                }))
            }
            Request::Read {
                name,
                history,
                unwrap,
                ansi,
                since_ms,
            } => {
                let term = {
                    let mut sessions = self.sessions.lock().unwrap();
                    named(&mut sessions, &name)?.term()
                };
                let rows = term.read(history, unwrap, ansi, since_ms);
                Ok(Response::Screen { rows })
            }
            Request::Clear { id, name } => {
                let (name, term) = {
                    let mut sessions = self.sessions.lock().unwrap();
                    let session = id_or_name(&mut sessions, id, name)?;
                    let name = session.name.clone();
                    ensure!(session.is_running(), "{name} isn't running");
                    (name, session.term())
                };
                term.clear()
                    .with_context(|| format!("{name} wasn't cleared"))?;
                Ok(Response::Done)
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
            Request::SearchMemory { dir, query, wanted } => {
                crate::plugins::ensure_enabled(&settings(), "memory")?;
                let project = memory::project_of(&dir);
                let embedder = embed::shared_now();
                let mut store = memory::Store::open(&self.socket)?;
                let entries = store.find(&project, &query, &wanted, embed::as_embed(&embedder))?;
                Ok(Response::Memory { entries })
            }
            Request::ListMemory { dir } => {
                crate::plugins::ensure_enabled(&settings(), "memory")?;
                let project = memory::project_of(&dir);
                let entries = memory::Memory::read(&self.socket, &project)?.listed();
                Ok(Response::Memory { entries })
            }
            Request::Remember {
                project,
                entry,
                replaces,
            } => {
                crate::plugins::ensure_enabled(&settings(), "memory")?;
                let embedder = embed::shared_now();
                let embedder = embed::as_embed(&embedder);
                let mut store = memory::Store::open(&self.socket)?;
                let (added, retired) = match &replaces {
                    Some(replacing) => {
                        store.replace(&project, entry, replacing.id, &replacing.why, embedder)?
                    }
                    None => (store.add_with(&project, entry, embedder)?, None),
                };
                for event in crate::events::remembered(&project, &added, retired.as_ref()) {
                    self.events.emit(event);
                }
                Ok(match replaces {
                    Some(_) => Response::Replaced {
                        added,
                        retired: retired.map(Box::new),
                    },
                    None => Response::Remembered(added),
                })
            }
            Request::NearMemory { dir } => {
                crate::plugins::ensure_enabled(&settings(), "memory")?;
                let project = memory::project_of(&dir);
                let embedder = embed::shared_now();
                let near = memory::near(&self.socket, &project, embed::as_embed(&embedder))?;
                Ok(Response::Near(near))
            }
            Request::DedupeMemory { dir, apply } => {
                crate::plugins::ensure_enabled(&settings(), "memory")?;
                let project = memory::project_of(&dir);
                let embedder = embed::shared_now();
                let merges =
                    memory::dedupe(&self.socket, &project, embed::as_embed(&embedder), apply)?;
                if apply {
                    for merge in &merges {
                        let ids: Vec<u64> = merge.merged.iter().map(|twin| twin.entry.id).collect();
                        let merged = Event::merged(project.clone(), merge.kept.clone(), &ids);
                        self.events.emit(merged);
                    }
                }
                Ok(Response::Deduped { merges })
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
            Request::Resources { client } => {
                // A look takes a moment, and the first waits half a second
                // for CPU to count: the sessions aren't held meanwhile.
                let running: Vec<(String, u32)> = {
                    let sessions = self.sessions.lock().unwrap();
                    let running = sessions
                        .iter()
                        .filter_map(|session| Some((session.name.clone(), session.running_pid()?)));
                    running.collect()
                };
                let whose = resources::Whose {
                    daemon: std::process::id(),
                    client,
                    sessions: &running,
                    warm: self.spare_pid(),
                };
                Ok(Response::Resources(resources::measure(
                    &self.earlier,
                    &whose,
                )))
            }
            Request::Spending => Ok(Response::Spending(protocol::Spending {
                today_usd: self.spending.today(),
                daily_budget_usd: settings().tasks.daily_budget_usd,
            })),
            Request::BacklogList { dir, all } => {
                let config = settings();
                backlog::ensure_enabled(&config)?;
                let project = project::of(&dir);
                let store = self.db.lock().unwrap().backlog(&project.path)?;
                // Each item's history: the tasks started for one.
                let tasks = if tasks::enabled(&config) {
                    self.tasks(&dir, false)
                } else {
                    Vec::new()
                };
                let tasks = tasks
                    .into_iter()
                    .filter(|task| task.record.backlog.is_some());
                Ok(Response::Backlog(Backlog {
                    project: project.name,
                    path: project.path,
                    items: store.items(all),
                    tasks: tasks.collect(),
                }))
            }
            Request::BacklogAdd {
                dir,
                text,
                body,
                tags,
            } => {
                let (number, item) = self.change_backlog(&dir, |store| {
                    let number = store.add(&text, &body, tags, now_seconds())?;
                    Ok((number, store.get(number).cloned()))
                })?;
                self.tell_backlog(Kind::BacklogAdded, &dir, item);
                Ok(Response::Added { number })
            }
            Request::BacklogEdit {
                dir,
                number,
                text,
                body,
                tags,
            } => {
                self.change_backlog(&dir, |store| {
                    store.edit(number, text.as_deref(), body.as_deref(), tags)
                })?;
                Ok(Response::Done)
            }
            Request::BacklogImport { dir, items } => {
                let (added, skipped, new) = self.change_backlog(&dir, |store| {
                    let (added, skipped) = store.import(&items, now_seconds())?;
                    let new: Vec<_> = added
                        .iter()
                        .filter_map(|n| store.get(*n).cloned())
                        .collect();
                    Ok((added, skipped, new))
                })?;
                for item in new {
                    self.tell_backlog(Kind::BacklogAdded, &dir, Some(item));
                }
                Ok(Response::Imported { added, skipped })
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
            } => {
                self.drop_spare();
                Ok(Response::Done)
            }
            Request::Shutdown {
                keep_sessions: false,
            } => {
                self.drop_spare();
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
                    errln!("crystal daemon: couldn't forget the sessions: {err:#}");
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
            errln!("crystal daemon: couldn't read the tasks waiting to start: {err:#}");
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
            errln!("crystal daemon: couldn't read the closed tasks: {err:#}");
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
        let pending = self.db.lock().unwrap().pending_task(id)?;
        let pending = pending.with_context(|| format!("there's no open task t{id}"))?;
        let project = project::of(&pending.cwd).path;
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
        {
            // Closed as it stops waiting to start, in one go, so that
            // whoever looks for it meanwhile finds it one way or the other;
            // and once, though it's cancelled twice at once.
            let mut db = self.db.lock().unwrap();
            ensure!(db.remove_pending_task(id)?, "there's no open task t{id}");
            if let Err(err) = db.record_task(&project, &cancelled) {
                errln!("crystal daemon: couldn't write down a closed task: {err:#}");
            }
        }
        let event = Event::pending_task(Kind::TaskClosed, project, cancelled);
        self.events.emit(event);
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
                    agent_names: false,
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
            errln!("crystal daemon: couldn't forget that t{id} waits to start: {err:#}");
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
        let brief = brief_of(&launch);
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
            agent_names: false,
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
            .emit(Event::started(&info, sessions[index].resumed()));
        self.events
            .emit(Event::about_session(Kind::SessionOpenedInTerminal, &info));
        Ok(Response::Created {
            name,
            task: task_id,
        })
    }

    fn new_session(&self, new: NewSession) -> Result<Response> {
        let mut sessions = self.sessions.lock().unwrap();
        let given = new.name.is_some();
        // The agent kept warm takes it on, when it was started the same way.
        let name = match self.take_spare(&new) {
            Some(spare) => spare::adopt(&mut sessions, spare, new)?,
            None => start(&mut sessions, &self.socket, new, None, None)?,
        };
        if given {
            named(&mut sessions, &name)?.keep_given_name();
        }
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
        self.events.emit(Event::started(&info, session.resumed()));
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
                Change::ToolUsed(tool) => Some(Event::tool_use(&info, tool)),
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
            Err(err) => match err.downcast::<drive::TimedOut>() {
                Ok(timed_out) => Some(Response::TimedOut {
                    message: timed_out.0,
                }),
                Err(err) => Some(Response::from(err)),
            },
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
        self.kill_at(&mut sessions, index);
        Ok(())
    }

    /// Kills the session at `index` in `sessions`, and takes it off the
    /// list.
    fn kill_at(&self, sessions: &mut Vec<Session>, index: usize) {
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
        // Only a move in the sidebar's order changes it, and needs the
        // projects with no sessions, which move too.
        let moves = matches!(order.command, layout::Command::SidebarMove { .. });
        let projects = if moves {
            self.known_projects()?
        } else {
            Vec::new()
        };
        let db = self.db.lock().unwrap();
        let (tabs, by_hand) = (db.ui(db::TABS)?, db.ui(db::ORDER)?);
        let kept = (tabs.as_deref(), by_hand.as_deref());
        let alone = crate::tui::obey_alone(infos, flows, projects, kept, order)
            .map_err(|why| anyhow!(why))?;
        db.keep_ui(db::TABS, &alone.tabs)?;
        if moves {
            db.keep_ui(db::ORDER, &alone.by_hand)?;
        }
        drop(db);
        for event in alone.events {
            self.events.emit(event);
        }
        for name in &alone.kill {
            self.kill(name)?;
        }
        Ok(alone.layout)
    }

    /// Runs an ended session's command again, in its directory and under
    /// its name, keeping its place in the list. An agent whose conversation
    /// can be picked up starts back in it, and a terminal crystal stopped
    /// idle in the directory its shell was in. One yet to start again after
    /// a restart, one that couldn't, and one crystal had stopped idle before
    /// it start now, as the restart would have started them. Its task is
    /// open again, the work going on, but for one crystal stopped idle,
    /// which carries on as if it had never stopped: its task, closed before
    /// it was stopped, stays closed.
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
        let mut ended = sessions.remove(index);
        let launch = ended.launch();
        let name_given = launch.name_given;
        // An agent picked up in its conversation has what it was shown of
        // its project's memory there still.
        let goes_on = launch.conversation.is_some() || launch.resume.is_some();
        // Run again, its task is open again: the work goes on.
        let backlog = launch.goal.as_ref().and_then(|goal| goal.backlog);
        let brief = brief_of(&launch);
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
                    agent_names: false,
                };
                // A terminal crystal stopped idle shows what it showed,
                // above its shell started again.
                let before = ended.stopped_idle().then(|| ended.term().kept_screen());
                // Woken, it's the same session, under its id: whoever
                // watches it, like `send --wait`, goes on watching it.
                let id = match ended.stopped_idle() {
                    true => ended.id.clone(),
                    false => new_id(),
                };
                start_as(
                    id,
                    &mut sessions,
                    &self.socket,
                    new,
                    launch.conversation,
                    launch.resume,
                    None,
                    before,
                )
            }
        };
        if let Err(err) = started {
            sessions.insert(index, ended);
            return Err(err);
        }
        // `start` adds the new session at the end; it goes where the old
        // one was. Whoever was looking at the old one is let go, to look
        // again at the one started, under its id or a new one.
        let mut started = sessions.pop().expect("start added a session");
        ended.term().close();
        if name_given {
            started.keep_given_name();
        }
        if goes_on {
            started
                .recalled()
                .carry_on(std::mem::take(ended.recalled()));
        }
        // It's the same task, open again, under the same number.
        if let (Some(goal), true) = (launch.goal, started.task_record().is_some()) {
            started.give_task(match ended.stopped_idle() {
                true => goal,
                false => TaskInfo {
                    waiting: false,
                    outcome: None,
                    ..goal
                },
            });
        }
        sessions.insert(index, started);
        self.tell_started(&sessions, name, Kind::TaskOpened);
        Ok(Response::Done)
    }
}

/// Stops the sessions that have sat idle for longer than `settings` allow:
/// agents, and terminals when they say so (see [`Session::idle_for`]),
/// unless what runs under them holds them (see [`Session::held_by`]),
/// which is looked at only then, once for all of them. They stay in the
/// list, ended, to start again where they were. Processes that can't be
/// looked at hold every one.
fn stop_idle_sessions(sessions: &mut [Session], settings: &Config) {
    let Some(limit) = settings.sessions.idle_limit() else {
        return;
    };
    let terminals = settings.sessions.stop_idle_terminals;
    let idle: Vec<usize> = (sessions.iter().enumerate())
        .filter(|(_, session)| {
            session
                .idle_for(terminals)
                .is_some_and(|idle| idle >= limit)
        })
        .map(|(index, _)| index)
        .collect();
    if idle.is_empty() {
        return;
    }
    let Some(processes) = resources::Processes::read() else {
        return;
    };
    for index in idle {
        let session = &mut sessions[index];
        let Some(pid) = session.running_pid() else {
            continue;
        };
        if !session.held_by(processes.under(pid)) {
            session.stop_idle();
        }
    }
}

/// Has every session keep the history `settings` say, the running ones
/// too, which let their oldest rows go when it's fewer.
fn follow_scrollback(sessions: &[Session], settings: &Config) {
    let lines = settings.scrollback_lines;
    vt::set_history_lines(lines);
    for session in sessions {
        session.term().keep_history(lines);
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
            let said = format!("nothing on {name}'s screen matched `{pattern}` after {seconds}s");
            return Err(drive::TimedOut(said).into());
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

/// Whether the distiller reads what `task` did as it closes: once it's
/// done or failed. A cancelled task did nothing anyone wanted kept.
fn read_as_it_closed(task: &TaskRecord) -> bool {
    matches!(task.state(), TaskState::Done | TaskState::Failed)
}

/// Tells of each entry of memory that goes stale, every file it's about
/// changed since it was said: looks as the daemon starts, then each time
/// `sweeps` asks and every [`STALE_SWEEP_EVERY`], while memory is on.
fn tell_stale(socket: &Path, events: &Bus, sweeps: &Receiver<()>) {
    loop {
        if memory::enabled_now() {
            match memory::newly_stale(socket) {
                Ok(stale) => {
                    for (project, entry) in stale {
                        events.emit(Event::memory(Kind::MemoryStale, project, entry));
                    }
                }
                Err(err) => errln!("crystal daemon: couldn't look at memory: {err:#}"),
            }
        }
        if let Err(RecvTimeoutError::Disconnected) = sweeps.recv_timeout(STALE_SWEEP_EVERY) {
            return;
        }
    }
}

/// What came of a pass of the distiller, for its event.
fn distill_about(report: &Result<distill::Report>) -> DistillAbout {
    match report {
        Ok(report) => DistillAbout {
            added: report.added.len(),
            again: report.again.len(),
            made_lessons: report.made_lessons.len(),
            rejected: report.rejected.len(),
            rechecked: report.kept.len() + report.reworded.len() + report.forgot.len(),
            superseded: report.updated.len() + report.retired.len(),
            cost_usd: report.cost_usd,
            failed: None,
        },
        Err(err) => DistillAbout {
            failed: Some(format!("{err:#}")),
            ..DistillAbout::default()
        },
    }
}

/// Tells of the entries the distiller added to the memory of `job`'s
/// project, and the notes it made lessons, by their ids, of those it
/// updated, retired or reworded, and of the stale ones it forgot.
fn tell_distilled(events: &Bus, job: &Job, report: &distill::Report) {
    for entry in &report.forgot_entries {
        let forgot = Event::memory(Kind::MemoryForgotten, job.project.clone(), entry.clone());
        events.emit(forgot);
    }
    for (was, holder) in &report.superseded {
        let superseded = Event::superseded(job.project.clone(), holder.clone(), was.clone());
        events.emit(superseded);
    }
    let Ok(mut store) = memory::Store::open(&job.socket) else {
        return;
    };
    let told = [
        (Kind::MemoryAdded, &report.added),
        (Kind::MemoryChanged, &report.made_lessons),
    ];
    for (kind, ids) in told {
        for &id in ids {
            if let Ok(Some(entry)) = store.get(&job.project, id) {
                events.emit(Event::memory(kind, job.project.clone(), entry));
            }
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

/// What the keep-up loop has kept of each terminal's screen in the
/// database: see [`Daemon::keep_screens`].
struct KeptScreens {
    /// Whether the database may hold screens: those a daemon before this
    /// one kept, until it has looked.
    maybe_some: bool,
    /// By each session's id.
    sessions: HashMap<String, KeptScreen>,
}

impl Default for KeptScreens {
    fn default() -> KeptScreens {
        KeptScreens {
            maybe_some: true,
            sessions: HashMap::new(),
        }
    }
}

impl KeptScreens {
    /// None at all: they've been forgotten.
    fn nothing() -> KeptScreens {
        KeptScreens {
            maybe_some: false,
            sessions: HashMap::new(),
        }
    }
}

/// What was last kept of a session's screen, or forgotten.
struct KeptScreen {
    /// The name it was kept under.
    name: String,
    /// The screen's [`Term::main_written`] as it was kept, and when; `None`
    /// when it was forgotten instead.
    kept: Option<(u64, Instant)>,
}

/// Whether a terminal started again from `saved` after a restart shows what
/// it showed before: not a task's, which draws its transcript again, nor an
/// agent's picked up in its conversation, which shows its own.
fn shows_again(saved: &SavedSession) -> bool {
    let resumes = saved
        .conversation
        .as_ref()
        .is_some_and(Conversation::can_resume);
    saved.task.is_none() && saved.resume.is_none() && !resumes
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
        None,
        None,
    )
}

/// Starts a session under the id `id` and adds it to `sessions`. Given a
/// `conversation`, an agent that can pick one up starts back in it; given a
/// `resume_command`, the command an agent said resumes it, it's resumed
/// with that instead. Never anywhere but its directory. An agent picked up
/// in its conversation after a move into another worktree is given
/// `moved`, which tells it so, as its next prompt: see [`moving`]. Given
/// `before`, what its terminal showed before a cold restart, it shows that
/// above its program, unless its agent is picked up in its conversation,
/// which shows its own.
#[allow(clippy::too_many_arguments)]
fn start_as(
    id: String,
    sessions: &mut Vec<Session>,
    socket: &Path,
    new: NewSession,
    conversation: Option<Conversation>,
    resume_command: Option<Vec<String>>,
    moved: Option<&str>,
    before: Option<vt::Saved>,
) -> Result<String> {
    let NewSession {
        name,
        cwd,
        command,
        env,
        task,
        backlog,
        brief,
        agent_names,
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
    // Claude Code, asked to with its first prompt, names it better still,
    // unless whoever started it holds on to the name it's given here.
    let agent_names = agent_names && names_itself(name.as_deref(), &command, &config);
    let (name, named_after_program) = name_for(sessions, name, task.as_deref(), program, &config)?;

    let rollouts = codex::Rollouts::for_session(&command, &cwd, &env);
    // An agent that said how to resume it comes back with that command:
    // typed into the session's shell, or else run in place of its command.
    // It says what it's doing again once it's up.
    let resume_command = resume_command.filter(|argv| resumable(argv, &name, &config, &cwd, &env));
    let at_a_shell = matches!(front::of_command(&command), Some(Front::Shell { .. }));
    let resumed = resume_command.is_some();
    let resumed_with = resume_command.as_ref().map(|argv| {
        let quoted: Vec<String> = argv.iter().map(|arg| crate::shell::quote(arg)).collect();
        quoted.join(" ")
    });
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
    // Picked up again with its own command, its conversation has heard
    // crystal's notes already.
    let (instructions, remembered) = if resumed {
        (Vec::new(), Vec::new())
    } else {
        launch_notes(socket, &cwd, &command, task.as_deref(), &brief, &config)
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
    let argv = match moved.filter(|_| resume.is_some()) {
        Some(notice) => agents::moved(argv, notice, &cwd),
        None => argv,
    };
    let before = before.filter(|_| resume.is_none() && !resumed);
    keep_scrollback();
    let mut session = Session::spawn(id, name.clone(), command, &argv, cwd, &env, before.as_ref())?;
    if let Some(typed) = typed
        && let Err(err) = session.term().write(&typed)
    {
        errln!("crystal daemon: couldn't resume {name}'s agent: {err:#}");
    }
    if let Some(picked_up) = resume
        .map(|id| format!("conversation {id}"))
        .or(resumed_with)
    {
        session.set_resumed(picked_up);
    }
    if named_after_program {
        session.mark_named_after_program();
    }
    if agent_names {
        session.let_agent_name();
    }
    session.recalled().launched(&remembered);
    session.set_about(&brief);
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

/// The name a new session takes beside `sessions`, and whether it's named
/// after its program: `name` when it's given and free; or else, with
/// `name_from_prompt` on, one for `task`, what it's asked to do; or else one
/// after its `program`, until its first prompt names it.
fn name_for(
    sessions: &[Session],
    name: Option<String>,
    task: Option<&str>,
    program: &str,
    config: &Config,
) -> Result<(String, bool)> {
    let from_prompt = task
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
    Ok((name, named_after_program))
}

/// Whether a new session started as `command`, given the name `name`, is
/// for its agent to name, as the settings say: Claude Code, with a name
/// nobody gave it.
fn names_itself(name: Option<&str>, command: &[String], config: &Config) -> bool {
    name.is_none() && config.name_by_agent && agents::program_name(command) == Some("claude")
}

/// What crystal tells an agent starting in `cwd` with `command` on top of
/// what it's asked (see [`notes`]): of `task`, the task it's given, with
/// tasks on, and of the pull request and the issue `brief` names, whether
/// it's a task or not; how to work on several things at once here and show
/// the user files, for Claude Code; the notes its worktree's sessions left; and what its
/// project remembers that has to do with the words of `command`. And the
/// entries of its project's memory that shows it, by id.
fn launch_notes(
    socket: &Path,
    cwd: &Path,
    command: &[String],
    task: Option<&str>,
    brief: &TaskBrief,
    config: &Config,
) -> (Vec<String>, Vec<u64>) {
    let about_task = paragraphs([
        task.map(|_| tasks::instructions(backlog::enabled(config))),
        tasks::forge_notes(brief, cwd, task.is_some()),
    ]);
    let parallel = (agents::program_name(command) == Some("claude"))
        .then(|| format!("{} {}", agents::PARALLEL_WORK, agents::SHOWING_FILES));
    let memory::Launch {
        text: remembered,
        shown,
    } = remembered(socket, cwd, command);
    let said = [
        task,
        about_task.as_deref(),
        parallel.as_deref(),
        remembered.as_deref(),
    ];
    let handoff = handoff_note(cwd, &said);
    (notes(about_task, parallel, handoff, remembered), shown)
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
        errln!("crystal daemon: couldn't resume {name}'s agent: command not found: {program}");
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
fn remembered(socket: &Path, cwd: &Path, command: &[String]) -> memory::Launch {
    let reader = match agents::program_name(command) {
        Some("claude") => memory::Reader::Claude,
        Some("codex") => memory::Reader::Agent,
        Some(_) if catalog::first_prompt_at(command).is_some() => memory::Reader::Agent,
        _ => return memory::Launch::default(),
    };
    if !memory::enabled_now() {
        return memory::Launch::default();
    }
    let asked = command[1..].join(" ");
    launch_memory(socket, cwd, &asked, reader)
}

/// What the memory of the project `cwd` is in tells `reader` as it starts
/// there, asked `asked`: what has to do with the files its worktree has
/// changed comes first.
fn launch_memory(socket: &Path, cwd: &Path, asked: &str, reader: memory::Reader) -> memory::Launch {
    let project = memory::project_of(cwd);
    let changed = git::branch_changes(cwd).unwrap_or_default();
    let embedder = embed::shared_now();
    let embedder = embed::as_embed(&embedder);
    memory::for_launch(socket, &project, asked, &changed, reader, embedder).unwrap_or_default()
}

/// What gives a Claude Code session in `cwd` the tools crystal tells it
/// to use, allowed up front so it never stops to ask for them: the crystal
/// commands its notes name, `crystal name` while crystal may ask it to name
/// its session, and with memory on, crystal's MCP server, run by `crystal`,
/// the path of this program, and its tools. Nothing for another program.
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
    // Asked to name its session, it does without stopping for the user.
    if config.name_by_agent {
        tools.push("Bash(crystal name:*)");
    }
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
/// driving sessions of its own, reading them, showing the user a file,
/// closing its task, noting something for later) doesn't stop for the user
/// at every step: a task in
/// the background that did would sit there with its work done, and one
/// driving workers would wait on each. A plugin's commands only while it's
/// on. What removes or cancels what's there (`crystal kill`, `worktree rm`,
/// `tasks cancel`, `flow cancel`, `backlog rm`, `memory rm`), what writes
/// in bulk from a file (`backlog import`), what's the user's to decide (a
/// flow's gate: `flow approve` and `back`), and what answers another
/// agent's question for it (`send-keys` and `answer`, which can say yes to
/// a permission) still ask.
///
/// A rule ending `:*` matches the command with any arguments or none, but
/// only as whole words: `crystal send:*` isn't `crystal send-keys`, and
/// `crystal task:*` isn't `crystal tasks cancel`.
fn crystal_commands(config: &Config) -> Vec<&'static str> {
    // Sessions: starting, driving and reading them, and what's on screen,
    // files shown there among it.
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
        "Bash(crystal layout)",
        "Bash(crystal layout --json)",
        "Bash(crystal layout export:*)",
        "Bash(crystal pane split:*)",
        "Bash(crystal pane close:*)",
        "Bash(crystal open:*)",
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
            "Bash(crystal backlog list:*)",
            "Bash(crystal backlog show:*)",
            "Bash(crystal backlog edit:*)",
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
            "Bash(crystal memory add:*)",
            "Bash(crystal memory list:*)",
            "Bash(crystal memory search:*)",
            "Bash(crystal memory show:*)",
            "Bash(crystal memory retire:*)",
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
        .then(|| launch_memory(socket, cwd, &spec.prompt, memory::Reader::Task).text)
        .flatten();
    // Its runs close it, so it's told nothing of closing it: only of the
    // pull request and the issue it's about.
    let about = tasks::forge_notes(brief, cwd, true);
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
    session.set_about(&brief);
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

/// What a session written down is about: its task's goal, criteria, pull
/// request and issue, or with no task, the pull request and the issue it's
/// about all the same.
fn brief_of(saved: &SavedSession) -> TaskBrief {
    match &saved.goal {
        Some(goal) => goal.brief.clone(),
        None => saved.about.clone(),
    }
}

/// The user's settings, read again each time so that a change counts at
/// once. A file that can't be read leaves the defaults.
fn settings() -> Config {
    Config::load().unwrap_or_default()
}

/// Has the screens made from now on keep the history the settings say,
/// and the diagrams in the transcripts drawn from now on drawn as they say.
fn keep_scrollback() {
    let settings = settings();
    vt::set_history_lines(settings.scrollback_lines);
    crate::mermaid::set_ascii(settings.mermaid_ascii);
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
    program: bool,
) -> Result<()> {
    let Found { name, id, term } = found;
    // No size keeps the one it has: a program watching it may not want to
    // change what the user sees.
    if rows > 0 && cols > 0 {
        term.resize(rows, cols)?;
    }
    let watch = term.watch(with_history, program);
    let running = watch.feed.is_some();
    let size = term.size();
    let attached = Response::Attached {
        name,
        id,
        running,
        size,
    };
    protocol::send(conn, &attached)?;
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
    fn a_terminal_shows_again_what_it_showed_unless_its_agent_picks_its_conversation_up() {
        let dir = tempfile::tempdir().unwrap();
        let shell = SavedSession {
            name: "shell".into(),
            command: vec!["zsh".into()],
            cwd: dir.path().to_path_buf(),
            conversation: None,
            task: None,
            goal: None,
            resume: None,
            about: TaskBrief::default(),
            name_given: false,
            moved: None,
            stopped_idle: false,
        };
        assert!(shows_again(&shell));
        // An agent whose conversation is there to pick up shows its own.
        let transcript = dir.path().join("talk.jsonl");
        let agent = SavedSession {
            command: vec!["claude".into()],
            conversation: Some(Conversation {
                id: "talk".into(),
                transcript: Some(transcript.clone()),
                prompted: false,
            }),
            ..shell.clone()
        };
        assert!(shows_again(&agent), "nothing to pick up yet");
        fs::write(&transcript, "{}\n").unwrap();
        assert!(!shows_again(&agent));
        // So does one resumed as it said, and a task, which draws its
        // transcript again.
        let reported = SavedSession {
            resume: Some(vec!["pi".into(), "--session".into(), "s1".into()]),
            ..shell.clone()
        };
        assert!(!shows_again(&reported));
        let task = SavedSession {
            task: Some(TaskSpec {
                prompt: "fix it".into(),
                args: Vec::new(),
            }),
            ..shell
        };
        assert!(!shows_again(&task));
    }

    #[test]
    fn claude_may_run_crystal_s_own_commands_but_not_those_that_remove_or_answer() {
        let mut config = Config::default();
        let rules = crystal_commands(&config);
        for allowed in [
            "Bash(crystal new:*)",
            "Bash(crystal send:*)",
            "Bash(crystal wait:*)",
            "Bash(crystal read:*)",
            "Bash(crystal open:*)",
            "Bash(crystal task:*)",
            "Bash(crystal flow run:*)",
            "Bash(crystal backlog done:*)",
            "Bash(crystal backlog show:*)",
            "Bash(crystal backlog edit:*)",
            "Bash(crystal memory search:*)",
            "Bash(crystal memory list:*)",
            "Bash(crystal memory add:*)",
            "Bash(crystal memory retire:*)",
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
            "crystal backlog import",
            "crystal memory rm",
            "crystal memory distill",
            "crystal memory promote",
            "crystal memory restore",
            "crystal memory reconcile",
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
