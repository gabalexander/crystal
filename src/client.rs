//! The CLI's side of the socket.

use crate::env;
use crate::git;
use crate::protocol::{self, NewSession, NewTask, Request, Response, SessionInfo, State, TaskSpec};
use crate::socket;
use anyhow::{Context, Result, bail};
use std::fs::OpenOptions;
use std::io::BufReader;
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
    let response = protocol::recv(BufReader::new(&conn))?.context(
        "the daemon hung up without answering; if crystal was just upgraded, \
         run `crystal restart-server`",
    )?;
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
    new_session_for(socket, name, cwd, command, Purpose::default())
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
) -> Result<String> {
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
    match ask(socket, &request, true)? {
        Some(Response::Created { name }) => Ok(name),
        _ => bail!("the daemon didn't create the session"),
    }
}

/// Asks the daemon to start a task in `cwd`, with this process's
/// environment, for backlog item `backlog` if it's for one. Starts the
/// daemon if it isn't running. Returns the task's name.
pub fn new_task(
    socket: &Path,
    name: Option<String>,
    cwd: PathBuf,
    spec: TaskSpec,
    backlog: Option<u64>,
) -> Result<String> {
    let request = Request::NewTask(NewTask {
        name,
        cwd,
        spec,
        env: env::current(),
        backlog,
    });
    match ask(socket, &request, true)? {
        Some(Response::Created { name }) => Ok(name),
        _ => bail!("the daemon didn't start the task"),
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
/// working on files that are gone. Sessions that had ended there leave the
/// list with it, since their directory is gone and they could never start
/// again.
pub fn remove_worktree(socket: &Path, path: &Path) -> Result<()> {
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
    git::remove_worktree(path)?;
    for session in ended {
        let kill = Request::Kill {
            name: session.name.clone(),
        };
        ask(socket, &kill, false)?;
    }
    Ok(())
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
    let shutdown = Request::Shutdown {
        keep_sessions: true,
    };
    if ask(socket, &shutdown, false)?.is_none() {
        return Ok(false);
    }
    // The old daemon takes its socket away as it goes.
    let deadline = Instant::now() + START_TIMEOUT;
    while socket.exists() {
        if Instant::now() > deadline {
            bail!("the old daemon didn't stop");
        }
        thread::sleep(Duration::from_millis(10));
    }
    start_daemon(socket)?;
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
