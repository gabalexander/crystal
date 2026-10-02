//! A program running in a PTY of its own.

use crate::agent_screen::{self, Looks, ScreenWatch};
use crate::protocol::{Activity, AgentEvent, SessionInfo, State};
use anyhow::Result;
use portable_pty::{CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use std::collections::BTreeMap;
use std::io::{self, ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How long a stopped session gets to exit after its hang-up before it's
/// killed outright.
pub const STOP_GRACE: Duration = Duration::from_secs(2);

/// Chunks of output a viewer may fall behind by before it's dropped.
const VIEWER_BACKLOG: usize = 256;

pub struct Session {
    pub name: String,
    /// As it was asked for, which is how `ls` shows it.
    command: Vec<String>,
    cwd: PathBuf,
    pid: Option<u32>,
    state: Arc<Mutex<State>>,
    /// `None` until an agent reports what it's doing; most programs never
    /// do.
    activity: Option<Activity>,
    /// What the screen has been saying the agent is doing.
    screen_watch: ScreenWatch,
    term: Arc<Term>,
}

impl Session {
    /// Starts `argv` in a PTY of its own. `command` is what was asked for;
    /// `argv` may add to it, like the flags that make an agent report what
    /// it's doing.
    pub fn spawn(
        name: String,
        command: Vec<String>,
        argv: &[String],
        cwd: PathBuf,
        env: &BTreeMap<String, String>,
    ) -> Result<Session> {
        let pty = native_pty_system().openpty(size(24, 80))?;
        let mut builder = CommandBuilder::new(&argv[0]);
        builder.args(&argv[1..]);
        builder.cwd(&cwd);
        builder.env_clear();
        for (key, value) in env {
            builder.env(key, value);
        }
        let mut child = pty.slave.spawn_command(builder)?;
        // Only the child may hold the terminal's other end, so that its exit
        // ends the output.
        drop(pty.slave);

        let output = pty.master.try_clone_reader()?;
        let term = Arc::new(Term {
            input: Mutex::new(pty.master.take_writer()?),
            pty: Mutex::new(pty.master),
            screen: Mutex::new(Screen {
                parser: vt100::Parser::new_with_callbacks(24, 80, 0, Callbacks::default()),
                viewers: Vec::new(),
                ended: false,
            }),
        });
        thread::spawn({
            let term = term.clone();
            move || term.pump(output)
        });

        let pid = child.process_id();
        let state = Arc::new(Mutex::new(State::Running));
        let exit = state.clone();
        thread::spawn(move || {
            let ended = match child.wait() {
                Ok(status) => ended(&status),
                Err(_) => State::Exited { code: 1 },
            };
            *exit.lock().unwrap() = ended;
        });

        Ok(Session {
            name,
            command,
            cwd,
            pid,
            state,
            activity: None,
            screen_watch: ScreenWatch::default(),
            term,
        })
    }

    pub fn is_running(&self) -> bool {
        *self.state.lock().unwrap() == State::Running
    }

    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            name: self.name.clone(),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            pid: self.pid,
            state: self.state.lock().unwrap().clone(),
            activity: self.activity,
        }
    }

    /// Works out what the agent is doing from what it just reported.
    pub fn on_agent_event(&mut self, event: AgentEvent) {
        self.activity = next_activity(self.activity, event, self.term.is_watched());
    }

    /// Reads what the agent is doing off the screen, and takes it as an
    /// event when that has changed.
    pub fn check_screen(&mut self) {
        if !self.is_running() {
            return;
        }
        let looks = self.term.looks();
        if let Some(event) = self.screen_watch.update(looks) {
            self.on_agent_event(event);
        }
    }

    /// Someone has just looked at the session.
    pub fn seen(&mut self) {
        if self.activity == Some(Activity::Done) {
            self.activity = Some(Activity::Idle);
        }
    }

    pub fn term(&self) -> Arc<Term> {
        self.term.clone()
    }

    /// Hangs up on everything the session started, the way closing a
    /// terminal window does, and kills whatever is still there after
    /// [`STOP_GRACE`].
    pub fn stop(&self) {
        let Some(pid) = self.pid.filter(|_| self.is_running()) else {
            return;
        };
        signal_group(pid, libc::SIGHUP);
        let state = self.state.clone();
        thread::spawn(move || {
            thread::sleep(STOP_GRACE);
            if *state.lock().unwrap() == State::Running {
                signal_group(pid, libc::SIGKILL);
            }
        });
    }
}

/// The daemon's end of a session's PTY: the screen the program has drawn,
/// the clients watching it, and the way in.
pub struct Term {
    pty: Mutex<Box<dyn MasterPty + Send>>,
    input: Mutex<Box<dyn Write + Send>>,
    screen: Mutex<Screen>,
}

struct Screen {
    parser: vt100::Parser<Callbacks>,
    viewers: Vec<Viewer>,
    /// The program has closed its end: there will be no more output.
    ended: bool,
}

struct Viewer {
    id: u64,
    feed: SyncSender<Arc<[u8]>>,
}

/// A new viewer's start: the screen as it is now, then everything the
/// program writes after it.
pub struct Watch {
    pub id: u64,
    pub screen: Vec<u8>,
    /// `None` once the program has ended.
    pub feed: Option<Receiver<Arc<[u8]>>>,
}

impl Term {
    pub fn watch(&self) -> Watch {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut screen = self.screen.lock().unwrap();
        let snapshot = screen.parser.screen().state_formatted();
        let feed = (!screen.ended).then(|| {
            let (feed, rx) = mpsc::sync_channel(VIEWER_BACKLOG);
            screen.viewers.push(Viewer { id, feed });
            rx
        });
        Watch {
            id,
            screen: snapshot,
            feed,
        }
    }

    /// What the screen says the agent is doing.
    pub fn looks(&self) -> Looks {
        let screen = self.screen.lock().unwrap();
        agent_screen::read(screen.parser.screen(), &screen.parser.callbacks().title)
    }

    pub fn is_watched(&self) -> bool {
        !self.screen.lock().unwrap().viewers.is_empty()
    }

    pub fn unwatch(&self, id: u64) {
        let mut screen = self.screen.lock().unwrap();
        screen.viewers.retain(|viewer| viewer.id != id);
    }

    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.input.lock().unwrap().write_all(bytes)
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let mut screen = self.screen.lock().unwrap();
        screen.parser.screen_mut().set_size(rows, cols);
        self.pty.lock().unwrap().resize(size(rows, cols))
    }

    /// Reads the program's output until it closes the terminal: keeps the
    /// screen up to date, passes the output on to every viewer, and answers
    /// the program's questions to its terminal.
    fn pump(&self, mut output: Box<dyn Read + Send>) {
        let mut buf = [0; 16 * 1024];
        loop {
            let n = match output.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let chunk: Arc<[u8]> = buf[..n].into();
            let replies = {
                let mut screen = self.screen.lock().unwrap();
                screen.parser.process(&chunk);
                // A viewer that's gone, or too far behind to catch up, is
                // dropped rather than holding up the program.
                screen
                    .viewers
                    .retain(|viewer| viewer.feed.try_send(chunk.clone()).is_ok());
                std::mem::take(&mut screen.parser.callbacks_mut().replies)
            };
            if !replies.is_empty() {
                let _ = self.write(&replies);
            }
        }
        let mut screen = self.screen.lock().unwrap();
        screen.ended = true;
        screen.viewers.clear();
    }
}

/// What vt100 hands back to us as it reads a program's output: questions
/// the program asks its terminal, and the title it gives it.
#[derive(Default)]
struct Callbacks {
    /// Answers to send back: where the cursor is, and what kind of terminal
    /// this is. Viewers only draw, so the answers come from here, whether
    /// anyone's watching or not.
    replies: Vec<u8>,
    /// Agents put a spinner here while they work.
    title: String,
}

impl vt100::Callbacks for Callbacks {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = String::from_utf8_lossy(title).into_owned();
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let param = params.first().and_then(|param| param.first()).copied();
        match (i1, c, param.unwrap_or(0)) {
            // Device status.
            (None, 'n', 5) => self.replies.extend_from_slice(b"\x1b[0n"),
            // Cursor position, 1-based.
            (None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                let _ = write!(self.replies, "\x1b[{};{}R", row + 1, col + 1);
            }
            // Primary device attributes: a VT100 with advanced video.
            (None, 'c', 0) => self.replies.extend_from_slice(b"\x1b[?1;2c"),
            // Secondary device attributes.
            (Some(b'>'), 'c', 0) => self.replies.extend_from_slice(b"\x1b[>0;0;0c"),
            _ => {}
        }
    }
}

/// What a session's agent is doing after `event`, given what it was doing
/// before and whether anyone is watching the session.
fn next_activity(before: Option<Activity>, event: AgentEvent, watched: bool) -> Option<Activity> {
    let after = match event {
        AgentEvent::Started => Activity::Idle,
        AgentEvent::TurnStarted | AgentEvent::ToolFinished => Activity::Working,
        AgentEvent::Asking => Activity::Waiting,
        AgentEvent::TurnEnded => Activity::Done,
        // Only news if we thought it was still working: a turn the user
        // cut short reports no end.
        AgentEvent::StillIdle if before == Some(Activity::Working) => Activity::Done,
        AgentEvent::StillIdle => return before,
    };
    // A turn that ends while someone's watching has been seen.
    if after == Activity::Done && watched {
        Some(Activity::Idle)
    } else {
        Some(after)
    }
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn ended(status: &ExitStatus) -> State {
    match status.signal() {
        // macOS spells it "Terminated: 15"; keep the name.
        Some(signal) => State::Signaled {
            signal: signal.split(':').next().unwrap_or(signal).to_string(),
        },
        None => State::Exited {
            code: status.exit_code(),
        },
    }
}

/// The child leads its own session and process group, so signalling the
/// group reaches whatever it started too.
fn signal_group(pid: u32, signal: libc::c_int) {
    // SAFETY: kill only sends a signal. The caller has checked the child
    // hasn't been reaped yet, so the group id still belongs to it.
    unsafe {
        libc::kill(-(pid as libc::pid_t), signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Activity::*;

    fn after(before: Option<Activity>, event: AgentEvent) -> Option<Activity> {
        next_activity(before, event, false)
    }

    #[test]
    fn a_turn_goes_from_working_to_done() {
        assert_eq!(after(None, AgentEvent::Started), Some(Idle));
        assert_eq!(after(Some(Idle), AgentEvent::TurnStarted), Some(Working));
        assert_eq!(after(Some(Working), AgentEvent::TurnEnded), Some(Done));
    }

    #[test]
    fn a_question_waits_until_the_agent_goes_on() {
        assert_eq!(after(Some(Working), AgentEvent::Asking), Some(Waiting));
        assert_eq!(
            after(Some(Waiting), AgentEvent::ToolFinished),
            Some(Working)
        );
    }

    #[test]
    fn a_turn_that_ends_while_watched_is_already_seen() {
        let activity = next_activity(Some(Working), AgentEvent::TurnEnded, true);
        assert_eq!(activity, Some(Idle));
    }

    #[test]
    fn sitting_idle_ends_a_turn_that_never_reported_its_end() {
        assert_eq!(after(Some(Working), AgentEvent::StillIdle), Some(Done));
        assert_eq!(after(Some(Idle), AgentEvent::StillIdle), Some(Idle));
        assert_eq!(after(Some(Waiting), AgentEvent::StillIdle), Some(Waiting));
    }
}
