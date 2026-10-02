//! The background process that owns every session. It outlives the
//! terminal it was started from, so sessions keep running when the
//! client goes away.

use crate::protocol::{self, Frame, Request, Response};
use crate::session::{STOP_GRACE, Session, Term};
use crate::socket;
use anyhow::{Context, Result, bail, ensure};
use std::io::{BufReader, ErrorKind, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{fs, process, thread};

pub fn run(socket: &Path) -> Result<()> {
    // Leave the client's terminal, so closing it doesn't hang up the
    // daemon. Fails harmlessly when run in the foreground from a shell.
    // SAFETY: setsid has no preconditions.
    unsafe {
        libc::setsid();
    }
    let listener = listen(socket)?;
    let daemon = Arc::new(Daemon {
        socket: socket.to_path_buf(),
        sessions: Mutex::default(),
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
    /// In the order they were created, which is the order `ls` shows.
    sessions: Mutex<Vec<Session>>,
}

impl Daemon {
    fn serve(&self, conn: UnixStream) -> Result<()> {
        let mut input = BufReader::new(&conn);
        let Some(request) = protocol::recv(&mut input)? else {
            return Ok(());
        };
        if let Request::Attach { name, rows, cols } = request {
            return match self.find(name.as_deref()) {
                Ok((name, term)) => attach(&conn, input, name, &term, rows, cols),
                Err(err) => Ok(protocol::send(&conn, &Response::from(err))?),
            };
        }
        let shutdown = matches!(request, Request::Shutdown);
        let response = self.handle(request).unwrap_or_else(Response::from);
        protocol::send(&conn, &response)?;
        if shutdown {
            let _ = fs::remove_file(&self.socket);
            process::exit(0);
        }
        Ok(())
    }

    /// The session called `name`, or the newest one.
    fn find(&self, name: Option<&str>) -> Result<(String, Arc<Term>)> {
        let sessions = self.sessions.lock().unwrap();
        let session = match name {
            Some(name) => sessions
                .iter()
                .find(|session| session.name == name)
                .with_context(|| format!("no session named {name}"))?,
            None => sessions.last().context("there are no sessions")?,
        };
        Ok((session.name.clone(), session.term()))
    }

    fn handle(&self, request: Request) -> Result<Response> {
        match request {
            Request::Attach { .. } => bail!("attach takes over the connection"),
            Request::New { name, cwd, command } => self.new_session(name, cwd, command),
            Request::List => {
                let sessions = self.sessions.lock().unwrap();
                Ok(Response::Sessions {
                    sessions: sessions.iter().map(Session::info).collect(),
                })
            }
            Request::Kill { name } => {
                let mut sessions = self.sessions.lock().unwrap();
                let index = sessions
                    .iter()
                    .position(|session| session.name == name)
                    .with_context(|| format!("no session named {name}"))?;
                sessions.remove(index).stop();
                Ok(Response::Done)
            }
            Request::Shutdown => {
                let sessions = std::mem::take(&mut *self.sessions.lock().unwrap());
                for session in &sessions {
                    session.stop();
                }
                let deadline = Instant::now() + STOP_GRACE + Duration::from_millis(500);
                while sessions.iter().any(Session::is_running) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(20));
                }
                Ok(Response::Done)
            }
        }
    }

    fn new_session(
        &self,
        name: Option<String>,
        cwd: PathBuf,
        command: Vec<String>,
    ) -> Result<Response> {
        ensure!(!command.is_empty(), "no command to run");
        ensure!(
            exists(&command[0], &cwd),
            "command not found: {}",
            command[0]
        );
        let mut sessions = self.sessions.lock().unwrap();
        let taken = |name: &str| sessions.iter().any(|session| session.name == name);
        let name = match name {
            Some(name) => {
                ensure!(
                    !name.is_empty() && !name.contains(char::is_whitespace),
                    "a session name can't be empty or contain spaces"
                );
                ensure!(!taken(&name), "a session named {name} already exists");
                name
            }
            None => unique_name(&command[0], taken),
        };
        let socket = self.socket.to_string_lossy();
        let env = [
            ("CRYSTAL_SOCKET", &*socket),
            ("CRYSTAL_SESSION", &*name),
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
        ];
        let session = Session::spawn(name.clone(), command, cwd, &env)?;
        sessions.push(session);
        Ok(Response::Created { name })
    }
}

/// Shows a session to a client until either of them goes: first the screen
/// as it is, then the output as it comes, while the client's keys and size
/// go to the session.
fn attach(
    conn: &UnixStream,
    mut input: BufReader<&UnixStream>,
    name: String,
    term: &Term,
    rows: u16,
    cols: u16,
) -> Result<()> {
    term.resize(rows, cols)?;
    let watch = term.watch();
    let running = watch.feed.is_some();
    protocol::send(conn, &Response::Attached { name, running })?;
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

impl From<anyhow::Error> for Response {
    fn from(err: anyhow::Error) -> Response {
        Response::Error {
            message: format!("{err:#}"),
        }
    }
}

/// Whether `program` names a file to run: a path, taken from `cwd`, or a
/// name found on `PATH`.
fn exists(program: &str, cwd: &Path) -> bool {
    if program.contains('/') {
        return cwd.join(program).is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
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
}
