//! The CLI's side of the socket.

use crate::protocol::{self, Request, Response};
use crate::socket;
use anyhow::{Context, Result, bail};
use std::fs::OpenOptions;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;
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
    protocol::send(&conn, request)?;
    let response =
        protocol::recv(BufReader::new(&conn))?.context("the daemon hung up without answering")?;
    match response {
        Response::Error { message } => bail!(message),
        response => Ok(Some(response)),
    }
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
