//! The CLI's side of the socket.

use crate::config::Config;
use crate::env;
use crate::events::{Event, Filter, Since};
use crate::forge::Checkout;
use crate::git;
use crate::handover;
use crate::layout::{self, Layout, Order};
use crate::protocol::{
    self, Backlog, NewSession, NewTask, PendingTask, Request, Response, TaskBrief, TaskSpec,
};
use crate::socket;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, ErrorKind};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How long a freshly started daemon gets to open its socket.
const START_TIMEOUT: Duration = Duration::from_secs(3);

/// Sends one request and waits for the answer. With `start`, a daemon is
/// started first if none is running; without it, `None` means there's no
/// daemon to ask.
pub fn ask(socket: &Path, request: &Request, start: bool) -> Result<Option<Response>> {
    let conn = match UnixStream::connect(socket) {
        Ok(conn) => conn,
        Err(_) if !start => return Ok(None),
        Err(_) => start_daemon(socket)?,
    };
    protocol::send_request(&conn, request)?;
    let response = protocol::recv(BufReader::new(&conn))?.ok_or_else(|| HungUp {
        crystal: socket::crystal_for(socket),
    })?;
    match response {
        Response::Error { message } => bail!(message),
        response => Ok(Some(response)),
    }
}

/// The daemon hung up without answering. A daemon from before requests
/// carried their version can't say that it's another version: it hangs up
/// on what it doesn't understand. A daemon handed over to a new crystal
/// hangs up on a wait it was in the middle of.
#[derive(Debug)]
pub struct HungUp {
    /// How `crystal` is run on this daemon ([`socket::crystal_for`]).
    crystal: String,
}

impl std::fmt::Display for HungUp {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "the daemon hung up without answering; if crystal was just upgraded, \
             run `{} restart-server`",
            self.crystal
        )
    }
}

impl std::error::Error for HungUp {}

/// Asks the daemon to start `command` in `cwd`, or the user's shell when
/// `command` is empty, with this process's environment. Starts the daemon
/// if it isn't running. Returns the new session's name.
pub fn new_session(
    socket: &Path,
    name: Option<String>,
    cwd: PathBuf,
    command: Vec<String>,
) -> Result<String> {
    let started = new_session_for(socket, name, cwd, command, Purpose::default())?;
    Ok(started.name)
}

/// A session the daemon has just started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub name: String,
    /// The number of the task it was started with, if it was.
    pub task: Option<u64>,
}

/// The session a request to start one started.
fn started(response: Option<Response>) -> Result<Started> {
    match response {
        Some(Response::Created { name, task }) => Ok(Started { name, task }),
        _ => bail!("the daemon didn't start it"),
    }
}

/// What a session is started to do, when it's started with something to
/// do: that makes it a task.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Purpose {
    /// The task, which is in the command already as the agent's first
    /// prompt.
    pub task: Option<String>,
    /// The backlog item it's for.
    pub backlog: Option<u64>,
    /// What the task carries beside its goal.
    pub brief: TaskBrief,
}

/// [`new_session`], for a session started with `purpose`.
pub fn new_session_for(
    socket: &Path,
    name: Option<String>,
    cwd: PathBuf,
    command: Vec<String>,
    purpose: Purpose,
) -> Result<Started> {
    new_session_with(socket, name, cwd, command, purpose, &[])
}

/// [`new_session_for`], with the variables `env` over this process's in
/// the session's environment. An empty `command` is the shell `[terminal]`
/// says, or the user's.
pub fn new_session_with(
    socket: &Path,
    name: Option<String>,
    cwd: PathBuf,
    mut command: Vec<String>,
    purpose: Purpose,
    env: &[(String, String)],
) -> Result<Started> {
    if command.is_empty() {
        // A config that can't be read is no reason not to start a shell.
        let terminal = Config::load().map(|config| config.terminal);
        command = terminal.unwrap_or_default().shell();
    }
    let mut environment = env::current();
    environment.extend(env.iter().cloned());
    let request = Request::New(NewSession {
        name,
        cwd,
        command,
        env: environment,
        task: purpose.task,
        backlog: purpose.backlog,
        brief: purpose.brief,
    });
    started(ask(socket, &request, true)?)
}

/// Asks the daemon to start a task in `cwd`, with this process's
/// environment, for backlog item `backlog` if it's for one, carrying
/// `brief`. Starts the daemon if it isn't running.
pub fn new_task(
    socket: &Path,
    name: Option<String>,
    cwd: PathBuf,
    spec: TaskSpec,
    backlog: Option<u64>,
    brief: TaskBrief,
) -> Result<Started> {
    let request = Request::NewTask(NewTask {
        name,
        cwd,
        spec,
        env: env::current(),
        backlog,
        brief,
    });
    started(ask(socket, &request, true)?)
}

/// Asks the daemon to keep `task` until it's started, and gives back its
/// number. Starts the daemon if it isn't running.
pub fn add_task(socket: &Path, task: PendingTask) -> Result<u64> {
    match ask(socket, &Request::AddTask(task), true)? {
        Some(Response::TaskAdded { id }) => Ok(id),
        _ => bail!("the daemon didn't take the task"),
    }
}

/// Asks the daemon to start task `id`, which waits to, with this process's
/// environment.
pub fn start_task(socket: &Path, id: u64) -> Result<Started> {
    let request = Request::StartTask {
        id,
        env: env::current(),
    };
    started(Some(ask_running(socket, &request)?))
}

/// Asks the daemon to start a run of the flow called `flow` on `goal`, in
/// `cwd`, with this process's environment. Starts the daemon if it isn't
/// running. Returns the run's name.
pub fn start_flow(socket: &Path, flow: &str, goal: &str, cwd: PathBuf) -> Result<String> {
    let request = Request::StartFlow {
        flow: flow.to_string(),
        goal: goal.to_string(),
        cwd,
        env: env::current(),
    };
    match ask(socket, &request, true)? {
        Some(Response::FlowStarted { run }) => Ok(run),
        _ => bail!("the daemon didn't start the flow"),
    }
}

/// Gives the session called `name` another name.
pub fn rename(socket: &Path, name: &str, new_name: &str) -> Result<()> {
    let request = Request::Rename {
        name: name.to_string(),
        new_name: new_name.to_string(),
    };
    ask_running(socket, &request)?;
    Ok(())
}

/// Closes the task of the session called `name`: done, or `failed`, with a
/// line on how it went.
pub fn close_task(socket: &Path, name: &str, failed: bool, summary: &str) -> Result<()> {
    let request = Request::Close {
        id: None,
        name: Some(name.to_string()),
        failed,
        summary: summary.to_string(),
        artifacts: Vec::new(),
    };
    ask_running(socket, &request)?;
    Ok(())
}

/// The backlog of the project `dir` is in: its open items, and with `all`
/// those done too. Starts the daemon if it isn't running.
pub fn backlog(socket: &Path, dir: PathBuf, all: bool) -> Result<Backlog> {
    match ask(socket, &Request::BacklogList { dir, all }, true)? {
        Some(Response::Backlog(backlog)) => Ok(backlog),
        _ => bail!("the daemon didn't send the backlog"),
    }
}

/// Runs the ended session called `name` again, with this process's
/// environment, the way [`new_session`] starts one.
pub fn respawn(socket: &Path, name: &str) -> Result<()> {
    let request = Request::Respawn {
        name: name.to_string(),
        env: env::current(),
    };
    ask_running(socket, &request)?;
    Ok(())
}

/// Has the daemon remove the worktree at `path`, unless sessions are still
/// running in it: removing a directory out from under a program would
/// leave it working on files that are gone. With `force`, changes not
/// committed go with it. Sessions that had ended there leave the list with
/// it, since their directory is gone and they could never start again.
/// Comes back once it's gone, or with why not: the daemon carries on with
/// it if this process goes first. Starts the daemon if it isn't running.
pub fn remove_worktree(socket: &Path, path: &Path, force: bool) -> Result<()> {
    let request = Request::RemoveWorktree {
        path: path.to_path_buf(),
        force,
    };
    match ask(socket, &request, true)? {
        Some(Response::Done) => Ok(()),
        _ => bail!("the daemon didn't say the worktree was removed"),
    }
}

/// Makes a worktree for `branch` in the repository `dir` is in, as
/// [`git::add_worktree`] does, and tells the daemon, for the plugins and
/// the hooks that listen for new worktrees. A new branch starts from
/// `base`, when it's given, or else where the settings say, fetched first.
/// The worktree goes in `path`, when it's given, or else where the
/// settings say.
pub fn add_worktree(
    socket: &Path,
    dir: &Path,
    branch: &str,
    base: Option<&str>,
    path: Option<PathBuf>,
) -> Result<PathBuf> {
    let path = git::add_worktree(dir, branch, &worktree_base(base), &worktree_location(path))?;
    tell_worktree(socket, &path, Some(branch.to_string()), true);
    Ok(path)
}

/// Makes a worktree on a new branch, `branch` or the first like it that's
/// free, as [`git::add_new_worktree`] does, and tells the daemon. It goes
/// in `path`, when it's given, or else where the settings say.
pub fn add_new_worktree(
    socket: &Path,
    dir: &Path,
    branch: &str,
    base: Option<&str>,
    path: Option<PathBuf>,
) -> Result<PathBuf> {
    let base = worktree_base(base);
    let (path, branch) = git::add_new_worktree(dir, branch, &base, &worktree_location(path))?;
    tell_worktree(socket, &path, Some(branch), true);
    Ok(path)
}

/// Where a new worktree's new branch starts: `named`, or the settings'
/// branch, fetched from `origin` first.
fn worktree_base(named: Option<&str>) -> git::Base {
    git::Base {
        named: named.map(String::from),
        configured: Config::load().unwrap_or_default().worktrees.base,
        fetch: true,
    }
}

/// Where a new worktree goes: `path`, or the settings' directory.
fn worktree_location(path: Option<PathBuf>) -> git::Location {
    git::Location {
        path,
        directory: Config::load().unwrap_or_default().worktrees.directory(),
    }
}

/// Has the daemon move the session called `name` into the worktree at
/// `path`, of its project, its agent picked up there in its conversation:
/// now, when its agent isn't in the middle of a turn, or else once its
/// turn ends. Says which. Starts the daemon if it isn't running.
pub fn move_session(socket: &Path, name: &str, path: &Path) -> Result<Moved> {
    let request = Request::MoveSession {
        name: name.to_string(),
        path: path.to_path_buf(),
    };
    match ask(socket, &request, true)? {
        Some(Response::Moved { later }) => Ok(if later { Moved::Later } else { Moved::Now }),
        Some(Response::Done) => Ok(Moved::AlreadyThere),
        _ => bail!("the daemon didn't say it moved {name}"),
    }
}

/// What [`move_session`] came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    /// The session started again in the worktree.
    Now,
    /// It moves once its agent's turn ends.
    Later,
    /// It was there already.
    AlreadyThere,
}

/// The worktree for a pull request: the one its project has on its branch
/// already, or else a new one, its commits fetched first, as
/// [`git::add_fetched_worktree`] makes it, which the daemon is told of.
pub fn pull_request_worktree(socket: &Path, checkout: &Checkout) -> Result<PathBuf> {
    let Checkout {
        project,
        branch,
        fetch,
    } = checkout;
    if let Some(path) = git::worktree_on(project, branch)? {
        return Ok(path);
    }
    let path = git::add_fetched_worktree(project, branch, fetch, &worktree_location(None))?;
    tell_worktree(socket, &path, Some(branch.clone()), true);
    Ok(path)
}

/// Tells the daemon a worktree was made or removed, starting it if it isn't
/// running: it runs the worktree hooks. The worktree is made or gone either
/// way, so a daemon that can't be told is no reason to fail.
fn tell_worktree(socket: &Path, path: &Path, branch: Option<String>, created: bool) {
    let event = Box::new(Event::worktree(created, path, branch.as_deref()));
    if let Err(err) = ask(socket, &Request::Emit { event }, true) {
        eprintln!(
            "crystal: couldn't tell the daemon about {}: {err:#}",
            path.display()
        );
    }
}

/// Tells the daemon about something done outside it, for its event log
/// and whoever listens. With no daemon running, there's nobody to tell.
pub fn tell(socket: &Path, event: Event) -> Result<()> {
    let event = Box::new(event);
    ask(socket, &Request::Emit { event }, false)?;
    Ok(())
}

/// Subscribes to the events `filter` takes, those in the log from `since`
/// first, then each new one as it happens. Starts the daemon if it isn't
/// running.
pub fn subscribe(socket: &Path, filter: Filter, since: Option<Since>) -> Result<Subscription> {
    let conn = match UnixStream::connect(socket) {
        Ok(conn) => conn,
        Err(_) => start_daemon(socket)?,
    };
    let mut subscription = Subscription {
        socket: socket.to_path_buf(),
        filter,
        since,
        input: BufReader::new(conn),
        line: Vec::new(),
        seq: 0,
        last: None,
    };
    subscription.seq = subscription.start(since)?;
    Ok(subscription)
}

/// Events from the daemon, as they happen.
pub struct Subscription {
    socket: PathBuf,
    filter: Filter,
    /// Where it asked to start in the log.
    since: Option<Since>,
    input: BufReader<UnixStream>,
    /// What has come of a line that isn't whole yet.
    line: Vec<u8>,
    /// The `seq` of the latest event before the subscription started.
    pub seq: u64,
    /// The `seq` of the latest event it has given: it picks up again after
    /// it on a new connection.
    last: Option<u64>,
}

impl Subscription {
    /// Asks for the events on the connection it has, from `since`, and
    /// gives the `seq` of the latest event before them.
    fn start(&mut self, since: Option<Since>) -> Result<u64> {
        let request = Request::Subscribe {
            filter: self.filter.clone(),
            since,
        };
        protocol::send_request(self.input.get_ref(), &request)?;
        match self.next_line(None)? {
            Some(Response::Subscribed { seq }) => Ok(seq),
            Some(Response::Error { message }) => bail!(message),
            _ => bail!("the daemon didn't start sending events"),
        }
    }

    /// The next event, waiting until `deadline` if there is one; `None`
    /// once it has passed. The daemon handing over to a new crystal cuts
    /// the stream, which carries on from the next one with nothing missed.
    /// The daemon going away, or dropping a subscriber that fell too far
    /// behind, is an error.
    pub fn next_before(&mut self, deadline: Option<Instant>) -> Result<Option<Event>> {
        let line = match self.next_line::<Value>(deadline) {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(None),
            Err(err) if err.is::<StreamEnded>() && self.pick_up_again()? => {
                return self.next_before(deadline);
            }
            Err(err) => return Err(err),
        };
        // Events are all there is after the start, but for the error that
        // ends a stream.
        if line["type"] == "error" {
            bail!(
                "{}",
                line["message"].as_str().unwrap_or("the daemon said no")
            );
        }
        let event: Event = serde_json::from_value(line)?;
        self.last = Some(event.seq);
        Ok(Some(event))
    }

    /// Subscribes again, on a new connection, after the last event given,
    /// or from where it started when it has given none, when a daemon is
    /// there to ask: one handed over to a new crystal cuts its streams.
    /// `false` when there's none; an error when it won't, say because it's
    /// a newer crystal now.
    fn pick_up_again(&mut self) -> Result<bool> {
        let Ok(conn) = UnixStream::connect(&self.socket) else {
            return Ok(false);
        };
        self.input = BufReader::new(conn);
        self.line.clear();
        let since = match self.last {
            Some(last) => Since::Seq(last),
            None => self.since.unwrap_or(Since::Seq(self.seq)),
        };
        self.start(Some(since))?;
        Ok(true)
    }

    /// The next line, read as a `T`; `None` once `deadline` has passed. A
    /// line cut short by the deadline is kept for the next call.
    fn next_line<T: serde::de::DeserializeOwned>(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<T>> {
        let timeout = match deadline {
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Ok(None);
                }
                Some(left)
            }
            None => None,
        };
        // A Mac refuses it on a connection the daemon has closed, as a
        // handover does: the read below gives what's left, then the end.
        match self.input.get_ref().set_read_timeout(timeout) {
            Err(err) if err.kind() != ErrorKind::InvalidInput => return Err(err.into()),
            _ => {}
        }
        match self.input.read_until(b'\n', &mut self.line) {
            Ok(_) if self.line.ends_with(b"\n") => {
                let line = std::mem::take(&mut self.line);
                Ok(Some(serde_json::from_slice(&line)?))
            }
            Ok(_) => Err(StreamEnded.into()),
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                Ok(None)
            }
            Err(err) => Err(err.into()),
        }
    }
}

/// The daemon stopped sending events, its connection closed.
#[derive(Debug)]
struct StreamEnded;

impl std::fmt::Display for StreamEnded {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("the daemon stopped sending events")
    }
}

impl std::error::Error for StreamEnded {}

impl Iterator for Subscription {
    type Item = Result<Event>;

    fn next(&mut self) -> Option<Result<Event>> {
        self.next_before(None).transpose()
    }
}

/// Has the TUI used last carry out a layout command, or with none open the
/// daemon, starting it if it isn't running, and gives the layout it came
/// to. A command run in a session says so, for one about "this session".
pub fn lay_out(socket: &Path, command: layout::Command) -> Result<Layout> {
    let order = Order {
        command,
        caller: env::own_session_id(socket),
    };
    match ask(socket, &Request::Layout(order), true)? {
        Some(Response::Layout(layout)) => Ok(layout),
        _ => bail!("the daemon didn't answer with the layout"),
    }
}

/// Asks a daemon that must be running already: the request is about a
/// session that has to exist.
fn ask_running(socket: &Path, request: &Request) -> Result<Response> {
    match ask(socket, request, false)? {
        Some(response) => Ok(response),
        None => bail!("no daemon is running on {}", socket.display()),
    }
}

/// How long a client waits for the daemon it asked to hand over to say it
/// has: the old one waits a few seconds for what it's doing, then the new
/// one reads what it was handed.
const HANDOVER_TIMEOUT: Duration = Duration::from_secs(30);

/// What restarting the daemon came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Restart {
    /// There was no daemon to restart.
    NoDaemon,
    /// It was handed over to this crystal, and `sessions` carried on.
    HandedOver { sessions: usize },
    /// It was stopped, and started again from this crystal, which started
    /// its running sessions again; `why`, when it was meant to be handed
    /// over.
    Cold { why: Option<String> },
}

/// Restarts the daemon on this program. That's how a newly installed
/// crystal takes over: until then, the daemon goes on running the program
/// it was started from. It's handed over to this one (see
/// [`crate::handover`]), its sessions carrying on, unless `cold`, or unless
/// it can't be: then it's stopped, keeping its list of running sessions,
/// and a new one started, which starts them again.
pub fn restart_daemon(socket: &Path, cold: bool) -> Result<Restart> {
    let why = if cold {
        None
    } else {
        match hand_over(socket)? {
            HandOver::NoDaemon => return Ok(Restart::NoDaemon),
            HandOver::Done { sessions } => return Ok(Restart::HandedOver { sessions }),
            HandOver::Refused(why) => Some(why),
        }
    };
    // A daemon that couldn't hand over may have gone already.
    if !stop_daemon(socket, true)? && why.is_none() {
        return Ok(Restart::NoDaemon);
    }
    start_daemon(socket)?;
    Ok(Restart::Cold { why })
}

/// What asking the daemon to hand over came to.
enum HandOver {
    NoDaemon,
    Done {
        sessions: usize,
    },
    /// It didn't hand over, for this reason.
    Refused(String),
}

/// Asks the daemon to hand over to this program.
fn hand_over(socket: &Path) -> Result<HandOver> {
    let Ok(conn) = UnixStream::connect(socket) else {
        return Ok(HandOver::NoDaemon);
    };
    let request = Request::Handover {
        exe: std::env::current_exe()?,
        format: handover::FORMAT,
    };
    // Before asking: once a daemon from before handovers has answered, and
    // closed the connection, a Mac refuses it.
    conn.set_read_timeout(Some(HANDOVER_TIMEOUT))?;
    protocol::send_request(&conn, &request)?;
    let answer = match protocol::recv(BufReader::new(&conn)) {
        Ok(Some(Response::HandedOver { sessions })) => return Ok(HandOver::Done { sessions }),
        Ok(Some(Response::Error { message })) => match message.strip_prefix("couldn't hand over: ")
        {
            Some(why) => why.to_string(),
            // What a daemon from before handovers says to a crystal of
            // another version.
            None => "the daemon was a crystal from before handovers".to_string(),
        },
        Ok(Some(_)) => "the daemon answered something else".to_string(),
        // The new crystal couldn't take over, or a daemon of this version
        // from before handovers didn't understand.
        Ok(None) | Err(_) => "the new daemon couldn't take them over".to_string(),
    };
    Ok(HandOver::Refused(answer))
}

/// Asks the daemon to stop, keeping its list of running sessions or not,
/// and waits until it has. It answers before it goes, and takes its socket
/// away as it goes, so until the socket is gone a command run next could
/// still reach it on its way out. Returns `false` when there was no daemon
/// to stop.
pub fn stop_daemon(socket: &Path, keep_sessions: bool) -> Result<bool> {
    let shutdown = Request::Shutdown { keep_sessions };
    if ask(socket, &shutdown, false)?.is_none() {
        return Ok(false);
    }
    let deadline = Instant::now() + START_TIMEOUT;
    while socket.exists() {
        if Instant::now() > deadline {
            bail!("the daemon didn't stop");
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(true)
}

fn start_daemon(socket: &Path) -> Result<UnixStream> {
    socket::prepare_dir(socket)?;
    let log_path = socket::log_path(socket);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    // Not waited on: the daemon outlives us, and init reaps it.
    Command::new(std::env::current_exe()?)
        .arg("--socket")
        .arg(socket)
        .arg("daemon")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .context("couldn't start the daemon")?;

    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match UnixStream::connect(socket) {
            Ok(conn) => return Ok(conn),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("the daemon didn't start; see {}", log_path.display())
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::os::unix::net::UnixListener;

    #[test]
    fn stopping_the_daemon_waits_until_its_socket_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("test.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let path = socket.clone();
        // A daemon that answers, then takes a moment to take its socket
        // away, as a real one does on its way out.
        let daemon = thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&conn).read_line(&mut request).unwrap();
            protocol::send(&conn, &Response::Done).unwrap();
            thread::sleep(Duration::from_millis(200));
            std::fs::remove_file(&path).unwrap();
        });
        assert!(stop_daemon(&socket, false).unwrap());
        assert!(!socket.exists());
        daemon.join().unwrap();
    }

    #[test]
    fn a_subscription_reads_what_came_before_the_daemon_hung_up() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("test.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (go, going) = std::sync::mpsc::channel();
        // A daemon that starts the stream, then sends an event and hangs
        // up, as one handing over does.
        let daemon = thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&conn).read_line(&mut request).unwrap();
            protocol::send(&conn, &Response::Subscribed { seq: 0 }).unwrap();
            going.recv().unwrap();
            let mut event = Event::handed_over("0.1.0", "0.2.0", 1);
            event.seq = 1;
            protocol::send(&conn, &event).unwrap();
        });
        let mut subscription = subscribe(&socket, Filter::default(), None).unwrap();
        go.send(()).unwrap();
        daemon.join().unwrap();
        // A Mac won't set a timeout on the connection now, but what came
        // before the end is read all the same.
        let event = subscription.next_before(None).unwrap().unwrap();
        assert_eq!(event.seq, 1);
    }

    #[test]
    fn stopping_no_daemon_says_there_was_none() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("test.sock");
        assert!(!stop_daemon(&socket, false).unwrap());
    }
}
