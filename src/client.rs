//! The CLI's side of the socket.

use crate::env;
use crate::events::{Event, Filter, Since};
use crate::forge::Checkout;
use crate::git;
use crate::protocol::{
    self, Backlog, NewSession, NewTask, PendingTask, Request, Response, SessionInfo, State,
    TaskSpec,
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
    // A daemon from before requests carried their version can't say that
    // it's another version: it hangs up on what it doesn't understand.
    let response = protocol::recv(BufReader::new(&conn))?.with_context(|| {
        format!(
            "the daemon hung up without answering; if crystal was just upgraded, \
             run `{} restart-server`",
            socket::crystal_for(socket)
        )
    })?;
    match response {
        Response::Error { message } => bail!(message),
        response => Ok(Some(response)),
    }
}

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
}

/// [`new_session`], for a session started with `purpose`.
pub fn new_session_for(
    socket: &Path,
    name: Option<String>,
    cwd: PathBuf,
    mut command: Vec<String>,
    purpose: Purpose,
) -> Result<Started> {
    if command.is_empty() {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        command.push(shell);
    }
    let request = Request::New(NewSession {
        name,
        cwd,
        command,
        env: env::current(),
        task: purpose.task,
        backlog: purpose.backlog,
    });
    started(ask(socket, &request, true)?)
}

/// Asks the daemon to start a task in `cwd`, with this process's
/// environment, for backlog item `backlog` if it's for one. Starts the
/// daemon if it isn't running.
pub fn new_task(
    socket: &Path,
    name: Option<String>,
    cwd: PathBuf,
    spec: TaskSpec,
    backlog: Option<u64>,
) -> Result<Started> {
    let request = Request::NewTask(NewTask {
        name,
        cwd,
        spec,
        env: env::current(),
        backlog,
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

/// Removes the worktree at `path`, unless sessions are still running in
/// it: removing a directory out from under a program would leave it
/// working on files that are gone. With `force`, changes not committed go
/// with it. Sessions that had ended there leave the list with it, since
/// their directory is gone and they could never start again.
pub fn remove_worktree(socket: &Path, path: &Path, force: bool) -> Result<()> {
    let sessions = match ask(socket, &Request::List, false)? {
        Some(Response::Sessions { sessions }) => sessions,
        _ => Vec::new(),
    };
    let (running, ended): (Vec<&SessionInfo>, Vec<&SessionInfo>) = sessions
        .iter()
        .filter(|session| runs_in(session, path))
        .partition(|session| session.state == State::Running);
    if !running.is_empty() {
        let names: Vec<&str> = running
            .iter()
            .map(|session| session.name.as_str())
            .collect();
        bail!("{} still running in {}", names.join(", "), path.display());
    }
    let branch = git::Checkout::find(path).and_then(|checkout| checkout.worktree().branch);
    git::remove_worktree(path, force)?;
    tell_worktree(socket, path, branch, false);
    for session in ended {
        let kill = Request::Kill {
            name: session.name.clone(),
        };
        ask(socket, &kill, false)?;
    }
    Ok(())
}

/// Makes a worktree for `branch` in the repository `dir` is in, as
/// [`git::add_worktree`] does, and tells the daemon, for the plugins that
/// listen for new worktrees.
pub fn add_worktree(socket: &Path, dir: &Path, branch: &str) -> Result<PathBuf> {
    let path = git::add_worktree(dir, branch)?;
    tell_worktree(socket, &path, Some(branch.to_string()), true);
    Ok(path)
}

/// Makes a worktree on a new branch, `branch` or the first like it that's
/// free, as [`git::add_new_worktree`] does, and tells the daemon.
pub fn add_new_worktree(socket: &Path, dir: &Path, branch: &str) -> Result<PathBuf> {
    let (path, branch) = git::add_new_worktree(dir, branch)?;
    tell_worktree(socket, &path, Some(branch), true);
    Ok(path)
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
    let path = git::add_fetched_worktree(project, branch, fetch)?;
    tell_worktree(socket, &path, Some(branch.clone()), true);
    Ok(path)
}

/// Tells the daemon a worktree was made or removed. The worktree is made or
/// gone either way, so a daemon that can't be told is no reason to fail.
fn tell_worktree(socket: &Path, path: &Path, branch: Option<String>, created: bool) {
    let event = Event::worktree(created, path, branch.as_deref());
    if let Err(err) = tell(socket, event) {
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
    protocol::send_request(&conn, &Request::Subscribe { filter, since })?;
    let mut subscription = Subscription {
        input: BufReader::new(conn),
        line: Vec::new(),
        seq: 0,
    };
    match subscription.next_line(None)? {
        Some(Response::Subscribed { seq }) => subscription.seq = seq,
        Some(Response::Error { message }) => bail!(message),
        _ => bail!("the daemon didn't start sending events"),
    }
    Ok(subscription)
}

/// Events from the daemon, as they happen.
pub struct Subscription {
    input: BufReader<UnixStream>,
    /// What has come of a line that isn't whole yet.
    line: Vec<u8>,
    /// The `seq` of the latest event before the subscription started.
    pub seq: u64,
}

impl Subscription {
    /// The next event, waiting until `deadline` if there is one; `None`
    /// once it has passed. The daemon going away, or dropping a
    /// subscriber that fell too far behind, is an error.
    pub fn next_before(&mut self, deadline: Option<Instant>) -> Result<Option<Event>> {
        let Some(line) = self.next_line::<Value>(deadline)? else {
            return Ok(None);
        };
        // Events are all there is after the start, but for the error that
        // ends a stream.
        if line["type"] == "error" {
            bail!(
                "{}",
                line["message"].as_str().unwrap_or("the daemon said no")
            );
        }
        Ok(Some(serde_json::from_value(line)?))
    }

    /// The next line, read as a `T`; `None` once `deadline` has passed. A
    /// line cut short by the deadline is kept for the next call.
    fn next_line<T: serde::de::DeserializeOwned>(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<T>> {
        let socket = self.input.get_ref();
        match deadline {
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Ok(None);
                }
                socket.set_read_timeout(Some(left))?;
            }
            None => socket.set_read_timeout(None)?,
        }
        match self.input.read_until(b'\n', &mut self.line) {
            Ok(0) => bail!("the daemon stopped sending events"),
            Ok(_) if self.line.ends_with(b"\n") => {
                let line = std::mem::take(&mut self.line);
                Ok(Some(serde_json::from_slice(&line)?))
            }
            Ok(_) => bail!("the daemon stopped sending events"),
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                Ok(None)
            }
            Err(err) => Err(err.into()),
        }
    }
}

impl Iterator for Subscription {
    type Item = Result<Event>;

    fn next(&mut self) -> Option<Result<Event>> {
        self.next_before(None).transpose()
    }
}

/// Whether `session` runs in the worktree at `path`.
fn runs_in(session: &SessionInfo, path: &Path) -> bool {
    session
        .worktree
        .as_ref()
        .is_some_and(|worktree| worktree.path == path)
}

/// Asks a daemon that must be running already: the request is about a
/// session that has to exist.
fn ask_running(socket: &Path, request: &Request) -> Result<Response> {
    match ask(socket, request, false)? {
        Some(response) => Ok(response),
        None => bail!("no daemon is running on {}", socket.display()),
    }
}

/// Stops the daemon, keeping its list of running sessions, and starts a
/// new one from this program, which starts those sessions again. That's
/// how a newly installed crystal takes over: until then, the daemon goes on
/// running the program it was started from. Returns `false` when there was
/// no daemon to restart.
pub fn restart_daemon(socket: &Path) -> Result<bool> {
    if !stop_daemon(socket, true)? {
        return Ok(false);
    }
    start_daemon(socket)?;
    Ok(true)
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
    fn stopping_no_daemon_says_there_was_none() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("test.sock");
        assert!(!stop_daemon(&socket, false).unwrap());
    }
}
