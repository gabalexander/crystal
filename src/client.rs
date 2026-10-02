//! The CLI's side of the socket.

use crate::env;
use crate::protocol::{self, NewSession, Request, Response};
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
    mut command: Vec<String>,
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
    });
    match ask(socket, &request, true)? {
        Some(Response::Created { name }) => Ok(name),
        _ => bail!("the daemon didn't create the session"),
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
