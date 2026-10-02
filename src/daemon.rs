//! The background process that owns every session. It outlives the
//! terminal it was started from, so sessions keep running when the
//! client goes away.

use crate::agents;
use crate::env;
use crate::protocol::{self, Conversation, Frame, NewSession, Request, Response};
use crate::session::{STOP_GRACE, Session, Term};
use crate::socket;
use crate::state::{self, SavedSession};
use crate::typing;
use anyhow::{Context, Result, bail, ensure};
use std::io::{BufReader, ErrorKind, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{fs, process, thread};

/// How often the daemon reads every session's screen for what its agent is
/// doing, and writes down the sessions that are running.
const KEEP_UP_EVERY: Duration = Duration::from_millis(250);

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
        state: state::path(socket),
        sessions: Mutex::default(),
    });
    daemon.start_saved_sessions();
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
    /// Where the running sessions are written down, to start them again
    /// after a restart.
    state: PathBuf,
    /// In the order they were created, which is the order `ls` shows.
    sessions: Mutex<Vec<Session>>,
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
                Ok((name, term)) => attach(&conn, input, name, &term, (rows, cols), history),
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
        for saved in state::load(&self.state) {
            let new = NewSession {
                name: Some(saved.name.clone()),
                cwd: saved.cwd,
                command: saved.command,
                env: env::current(),
            };
            if let Err(err) = start(&mut sessions, &self.socket, new, saved.conversation) {
                eprintln!(
                    "crystal daemon: couldn't start {} again: {err:#}",
                    saved.name
                );
            }
        }
    }

    /// Again and again: reads every session's screen for what its agent is
    /// doing, and writes down the running sessions when they've changed.
    fn keep_up(&self) {
        let mut last_saved: Vec<SavedSession> = Vec::new();
        loop {
            thread::sleep(KEEP_UP_EVERY);
            let mut sessions = self.sessions.lock().unwrap();
            for session in sessions.iter_mut() {
                session.check_screen();
            }
            // Written while the list is still locked, so that an older list
            // can never be written after a shutdown has emptied it.
            let saved: Vec<SavedSession> = sessions.iter().filter_map(Session::saved).collect();
            if saved != last_saved {
                match state::save(&self.state, &saved) {
                    Ok(()) => last_saved = saved,
                    Err(err) => eprintln!("crystal daemon: couldn't save the sessions: {err:#}"),
                }
            }
        }
    }

    /// The session called `name`, or the newest one, for a client about to
    /// show it. That counts as having seen it.
    fn find(&self, name: Option<&str>) -> Result<(String, Arc<Term>)> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = match name {
            Some(name) => named(&mut sessions, name)?,
            None => sessions.last_mut().context("there are no sessions")?,
        };
        session.seen();
        Ok((session.name.clone(), session.term()))
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
            Request::List => {
                let sessions = self.sessions.lock().unwrap();
                Ok(Response::Sessions {
                    sessions: sessions.iter().map(Session::info).collect(),
                })
            }
            Request::Report {
                name,
                event,
                conversation,
            } => {
                let mut sessions = self.sessions.lock().unwrap();
                let session = named(&mut sessions, &name)?;
                session.on_agent_event(event);
                if let Some(conversation) = conversation {
                    session.set_conversation(conversation);
                }
                Ok(Response::Done)
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
            Request::Send { name, text, enter } => {
                let term = self.running_term(&name)?;
                term.write(&typing::keystrokes(&text, term.wants_bracketed_paste()))?;
                if enter {
                    thread::sleep(typing::ENTER_PAUSE);
                    term.write(typing::ENTER)?;
                }
                Ok(Response::Done)
            }
            Request::Read { name, history } => {
                let mut sessions = self.sessions.lock().unwrap();
                let rows = named(&mut sessions, &name)?.term().rows(history);
                Ok(Response::Screen { rows })
            }
            // Leaving is enough: the sessions' terminals close with the
            // daemon, and the list of them stays as it was last written.
            Request::Shutdown {
                keep_sessions: true,
            } => Ok(Response::Done),
            Request::Shutdown {
                keep_sessions: false,
            } => {
                let sessions = std::mem::take(&mut *self.sessions.lock().unwrap());
                for session in &sessions {
                    session.stop();
                }
                let deadline = Instant::now() + STOP_GRACE + Duration::from_millis(500);
                while sessions.iter().any(Session::is_running) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(20));
                }
                // Asked to stop, the sessions stay stopped.
                state::forget(&self.state);
                Ok(Response::Done)
            }
        }
    }

    fn new_session(&self, new: NewSession) -> Result<Response> {
        let mut sessions = self.sessions.lock().unwrap();
        let name = start(&mut sessions, &self.socket, new, None)?;
        Ok(Response::Created { name })
    }
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
            ensure!(
                !name.is_empty() && !name.contains(char::is_whitespace),
                "a session name can't be empty or contain spaces"
            );
            ensure!(!taken(&name), "a session named {name} already exists");
            name
        }
        None => unique_name(program, taken),
    };

    let env = env::for_session(&env, &name, socket);
    let crystal = std::env::current_exe()?;
    let resume = conversation
        .as_ref()
        .filter(|conversation| conversation.can_resume())
        .map(|conversation| conversation.id.as_str());
    let argv = agents::argv(&command, &crystal, resume);
    let mut session = Session::spawn(name.clone(), command, &argv, cwd, &env)?;
    if let Some(conversation) = conversation {
        session.set_conversation(conversation);
    }
    sessions.push(session);
    Ok(name)
}

fn named<'a>(sessions: &'a mut [Session], name: &str) -> Result<&'a mut Session> {
    sessions
        .iter_mut()
        .find(|session| session.name == name)
        .with_context(|| format!("no session named {name}"))
}

/// Shows a session to a client until either of them goes: first the screen
/// as it is (after its history, with `with_history`), then the output as
/// it comes, while the client's keys and size go to the session.
fn attach(
    conn: &UnixStream,
    mut input: BufReader<&UnixStream>,
    name: String,
    term: &Term,
    (rows, cols): (u16, u16),
    with_history: bool,
) -> Result<()> {
    term.resize(rows, cols)?;
    let watch = term.watch(with_history);
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
