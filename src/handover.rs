//! Restarting the daemon on a newly installed crystal without stopping what
//! runs in it: what `crystal restart-server` does, unless it's asked to
//! restart the daemon cold.
//!
//! The daemon runs the new crystal in its own process, with `exec`. The pid
//! stays the same, so every program it started is still its child, and
//! waiting for one still says how it ended; and the descriptors it keeps
//! open across the exec stay open: the listening socket, which never
//! closes, so a client that connects meanwhile only waits; each session's
//! terminal; and a background task's pipes to its `claude`. The rest of the
//! daemon's state, each session's screen and history among it, goes in a
//! file the new crystal reads as it starts ([`State`]). It's unlinked as
//! soon as it's made, and passed on as an open descriptor, so it's never
//! left on disk, though it holds each session's environment.
//!
//! What can be refused is refused before anything changes: a crystal that
//! reads another [`FORMAT`], or isn't there to run. The client then
//! restarts the daemon cold, the way it always has. Once the daemon has
//! begun there's no going back: if the exec fails, it stops as a shutdown
//! that keeps its sessions does, and the client starts the next daemon,
//! which starts them again from the database, where they were written down
//! first. So does a new crystal that can't read what it was handed.
//!
//! herdr hands over by passing descriptors over a socket to a new process
//! instead. That keeps the old daemon alive until the new one has taken
//! over, but its programs stop being the daemon's children, so how they end
//! is lost, and it needs a second socket and a handshake. Exec keeps both
//! simple.

use crate::flow_run::FlowRun;
use crate::report;
use crate::session;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{self, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What a handover's [`State`] is written as. A daemon only hands over to a
/// crystal that reads the same; one more each time [`State`], or anything
/// in it, changes in a way an older crystal couldn't read.
pub const FORMAT: u32 = 1;

/// Everything a daemon hands the next: what it takes to carry each session
/// on, and the descriptors, by number, that it kept open across the exec.
#[derive(Serialize, Deserialize)]
pub struct State {
    /// The version of the crystal that handed over.
    pub from: String,
    /// The socket clients connect to.
    pub listener: RawFd,
    /// The connections the handover was asked on: the new daemon answers
    /// them.
    pub asking: Vec<RawFd>,
    /// Connections that came in while the daemon handed over, not read
    /// from yet.
    pub waiting: Vec<RawFd>,
    /// In the order `ls` shows them.
    pub sessions: Vec<session::Handed>,
    pub flows: Vec<HandedFlow>,
    /// The worktrees the daemon was removing: the new daemon finishes
    /// removing them, and answers the clients that asked.
    #[serde(default)]
    pub removals: Vec<HandedRemoval>,
    /// The sessions on their way into other worktrees: the new daemon
    /// moves them.
    #[serde(default)]
    pub moves: Vec<HandedMove>,
    /// The tokens reported for each project, by its main worktree.
    #[serde(default)]
    pub project_tokens: HashMap<PathBuf, report::Shown>,
}

/// A flow run as it's handed over: with the environment its steps start
/// with, which isn't written down anywhere else.
#[derive(Serialize, Deserialize)]
pub struct HandedFlow {
    pub run: FlowRun,
    pub env: BTreeMap<String, String>,
}

impl HandedFlow {
    pub fn of(run: &FlowRun) -> HandedFlow {
        HandedFlow {
            run: run.clone(),
            env: run.env.clone(),
        }
    }

    pub fn taken_over(self) -> FlowRun {
        FlowRun {
            env: self.env,
            ..self.run
        }
    }
}

/// A worktree the daemon was removing as it handed over.
#[derive(Serialize, Deserialize)]
pub struct HandedRemoval {
    pub path: PathBuf,
    /// Its repository's main worktree.
    pub project: PathBuf,
    pub branch: Option<String>,
    pub force: bool,
    /// git, if it was still removing the worktree: this process's child
    /// still, which the new daemon waits for, then reaps.
    pub git: Option<u32>,
    /// The connections waiting to hear it's done.
    pub asking: Vec<RawFd>,
}

/// A session the daemon was moving into another worktree as it handed
/// over.
#[derive(Serialize, Deserialize)]
pub struct HandedMove {
    /// The session's id.
    pub session: String,
    /// The worktree it moves into.
    pub path: PathBuf,
    pub branch: Option<String>,
    /// Whether its program had been stopped, to start again in the
    /// worktree once it has ended.
    pub stopping: bool,
}

/// Refuses to hand over to the crystal at `exe` when it couldn't take
/// over: it reads handovers of another `format`, or isn't a program to
/// run. Nothing has changed yet, so the daemon can be restarted cold.
pub fn check(exe: &Path, format: u32) -> Result<()> {
    ensure!(
        format == FORMAT,
        "the new crystal reads handovers of another kind ({format}, where this daemon writes {FORMAT})"
    );
    let runnable = exe
        .metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0);
    ensure!(runnable, "{} isn't a program to run", exe.display());
    Ok(())
}

/// Writes `state` to a file in `dir` that's unlinked straight away, and
/// gives it back open, read from the start, for the new crystal to read.
pub fn write(dir: &Path, state: &State) -> Result<OwnedFd> {
    let path = dir.join(format!(".handover-{}", process::id()));
    // Left behind by a daemon of the same pid that died halfway.
    let _ = fs::remove_file(&path);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("couldn't make {}", path.display()))?;
    fs::remove_file(&path)?;
    let mut out = BufWriter::new(file);
    serde_json::to_writer(&mut out, state)?;
    let mut file = out.into_inner().map_err(|err| err.into_error())?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file.into())
}

/// Reads the state the last daemon handed over on the descriptor `fd`.
pub fn read(fd: RawFd) -> Result<State> {
    let file = File::from(inherit(fd).context("there's no handover to read")?);
    let state = serde_json::from_reader(BufReader::new(file))
        .context("couldn't read what the last daemon handed over")?;
    Ok(state)
}

/// Keeps `fd` open across the exec, and gives its number, which the next
/// crystal finds it by. Whatever owns it here must stay until the exec.
pub fn keep_across_exec(fd: BorrowedFd) -> io::Result<RawFd> {
    close_on_exec(fd.as_raw_fd(), false)?;
    Ok(fd.as_raw_fd())
}

/// Takes the descriptor numbered `fd`, which the last daemon kept open
/// across the exec, and has it closed on the next exec again, like every
/// other.
pub fn inherit(fd: RawFd) -> io::Result<OwnedFd> {
    // Fails for a number that isn't open.
    close_on_exec(fd, true)?;
    // SAFETY: the descriptor is open, as fcntl just found, and nothing
    // else in this process owns it: its owner went with the exec.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn close_on_exec(fd: RawFd, close: bool) -> io::Result<()> {
    // SAFETY: fcntl only reads and sets the descriptor's flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let flags = if close {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Runs the crystal at `exe` in this process, as the daemon on `socket`,
/// to take over from the state on the descriptor `state`. Only comes back
/// when that fails.
pub fn exec(exe: &Path, socket: &Path, state: RawFd) -> io::Error {
    Command::new(exe)
        .arg("--socket")
        .arg(socket)
        .arg("daemon")
        .arg("--handover")
        .arg(state.to_string())
        // A daemon the tests have pretend to be another version becomes
        // the crystal it's handed to.
        .env_remove("CRYSTAL_PRETEND_VERSION")
        .exec()
}

/// Begins a handover: from now on, the daemon starts nothing on the side
/// that it would only have to stop, like a plugin's hook.
pub fn begin() {
    UNDERWAY.store(true, Ordering::SeqCst);
}

/// Whether a handover has begun.
pub fn underway() -> bool {
    UNDERWAY.load(Ordering::SeqCst)
}

static UNDERWAY: AtomicBool = AtomicBool::new(false);

/// What reading a terminal or a pipe came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Got {
    Bytes(usize),
    /// The other end has closed, or reading failed: nothing more comes.
    End,
    /// A handover has stopped the reading: what's still to be read stays
    /// where it is, for the next daemon.
    Stopped,
}

/// Stops the daemon's readers, of its terminals and its tasks' pipes, all
/// at once: each is waiting on what it reads and on this, and stops as
/// soon as this is stopped, without reading another byte.
pub struct Readers {
    /// A pipe written once to stop, and never read, so it stays readable.
    stop: (File, File),
}

impl Readers {
    pub fn new() -> io::Result<Readers> {
        let mut fds = [0; 2];
        // SAFETY: pipe fills in two descriptors, which are then owned here.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both were just made, and nothing else owns them.
        let stop = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
        close_on_exec(fds[0], true)?;
        close_on_exec(fds[1], true)?;
        Ok(Readers { stop })
    }

    /// Reads what's there in `source` into `buf`, waiting for some, unless
    /// the readers have been stopped.
    pub fn read(&self, source: &File, buf: &mut [u8]) -> io::Result<Got> {
        loop {
            let mut fds = [
                libc::pollfd {
                    fd: source.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.stop.0.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: two valid pollfds, and a count of two.
            if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } == -1 {
                let err = io::Error::last_os_error();
                if err.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if fds[1].revents != 0 {
                return Ok(Got::Stopped);
            }
            if fds[0].revents == 0 {
                continue;
            }
            return match (&*source).read(buf) {
                Ok(0) => Ok(Got::End),
                Ok(n) => Ok(Got::Bytes(n)),
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                // A terminal whose program has closed it says so with an
                // error, on Linux.
                Err(_) => Ok(Got::End),
            };
        }
    }

    pub fn stop(&self) {
        let _ = (&self.stop.1).write_all(&[1]);
    }
}

/// The daemon's readers: see [`Readers`].
pub fn readers() -> &'static Readers {
    static READERS: OnceLock<Readers> = OnceLock::new();
    READERS.get_or_init(|| Readers::new().expect("a pipe to stop the readers"))
}

/// Stops the daemon's readers, for good: what's still to be read is the
/// next daemon's.
pub fn stop_reading() {
    readers().stop();
}

/// The daemon's door. Each connection it takes is counted in until it has
/// been answered, or has turned into a stream, an attach or events, that a
/// handover cuts and whose client comes back; so a handover can wait for
/// the answers. Once a handover has begun, the daemon takes no more: the
/// one it takes as it learns that waits unread, with those still in the
/// socket's backlog, for the next crystal to answer.
#[derive(Default)]
pub struct Gate {
    door: Mutex<Door>,
    changed: Condvar,
}

#[derive(Default)]
struct Door {
    closing: bool,
    /// Connections taken and not answered yet.
    answering: usize,
    /// The daemon has stopped taking connections.
    shut: bool,
    waiting: Vec<UnixStream>,
}

/// A connection counted in at the daemon's [`Gate`]: dropped once it has
/// been answered.
pub struct Ticket(Arc<Gate>);

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut door = self.0.door.lock().unwrap();
        door.answering -= 1;
        self.0.changed.notify_all();
    }
}

impl Gate {
    /// Takes a connection the daemon has just accepted: counted in, or
    /// once a handover has begun, kept unread for the next crystal, and
    /// `None`: the daemon takes no more.
    pub fn admit(self: &Arc<Gate>, conn: UnixStream) -> Option<(UnixStream, Ticket)> {
        let mut door = self.door.lock().unwrap();
        if door.closing {
            door.waiting.push(conn);
            door.shut = true;
            self.changed.notify_all();
            return None;
        }
        door.answering += 1;
        Some((conn, Ticket(self.clone())))
    }

    /// Closes the door of the daemon listening on `socket`, and waits until
    /// every connection it took before has been answered, and it has
    /// stopped taking them, or until `deadline`. Gives back the connections
    /// that came in meanwhile.
    pub fn close(&self, socket: &Path, deadline: Instant) -> Vec<UnixStream> {
        self.door.lock().unwrap().closing = true;
        // Wakes the daemon from waiting for a connection, to find the door
        // closed. It's answered by the next crystal, which finds it empty.
        let _wake = UnixStream::connect(socket);
        let door = self.door.lock().unwrap();
        let left = deadline.saturating_duration_since(Instant::now());
        let (mut door, _) = self
            .changed
            .wait_timeout_while(door, left, |door| door.answering > 0 || !door.shut)
            .unwrap();
        std::mem::take(&mut door.waiting)
    }
}

/// The processes the daemon runs beside its sessions, while they run: a
/// plugin's hook, or the distiller's `claude`. A handover gives them a
/// moment to finish, then stops them, so none is left without the daemon
/// that started it to reap it.
pub struct Helpers {
    running: Mutex<Vec<u32>>,
    ended: Condvar,
}

/// A helper counted in, until whoever started it has reaped it and drops
/// this.
pub struct Helper {
    helpers: &'static Helpers,
    pid: u32,
}

/// The daemon's helpers.
pub static HELPERS: Helpers = Helpers::new();

/// How long the helpers still running get, once they're stopped, for
/// whoever started each to see it end.
const REAPED_WITHIN: Duration = Duration::from_secs(1);

impl Helpers {
    const fn new() -> Helpers {
        Helpers {
            running: Mutex::new(Vec::new()),
            ended: Condvar::new(),
        }
    }

    /// Counts in the process `pid`, which leads a process group of its own.
    pub fn started(&'static self, pid: u32) -> Helper {
        self.running.lock().unwrap().push(pid);
        Helper { helpers: self, pid }
    }

    /// Waits until every helper has finished, or until `deadline`, then
    /// stops those still running, and waits a moment more for them to be
    /// reaped.
    pub fn finish(&self, deadline: Instant) {
        let left = deadline.saturating_duration_since(Instant::now());
        let running = self.running.lock().unwrap();
        let (running, _) = self
            .ended
            .wait_timeout_while(running, left, |running| !running.is_empty())
            .unwrap();
        for &pid in running.iter() {
            eprintln!("crystal daemon: stopped {pid}, which was still running, to hand over");
            session::signal_group(pid, libc::SIGKILL);
        }
        let _ = self
            .ended
            .wait_timeout_while(running, REAPED_WITHIN, |running| !running.is_empty())
            .unwrap();
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let mut running = self.helpers.running.lock().unwrap();
        running.retain(|&pid| pid != self.pid);
        self.helpers.ended.notify_all();
    }
}

/// Waits until the child `pid` has ended, without reaping it: whoever
/// reaps it can first make sure a handover isn't taking it over.
pub fn wait_for_end(pid: u32) -> io::Result<()> {
    loop {
        // SAFETY: zeroed is a valid siginfo_t, which waitid fills in.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let options = libc::WEXITED | libc::WNOWAIT;
        // SAFETY: info is a valid siginfo_t to fill in.
        if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, options) } == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Waits for the child `pid` to end, and reaps it: how it ended.
pub fn reap(pid: u32) -> io::Result<ExitStatus> {
    loop {
        let mut status = 0;
        // SAFETY: status is a valid int to fill in.
        if unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) } != -1 {
            return Ok(ExitStatus::from_raw(status));
        }
        let err = io::Error::last_os_error();
        if err.kind() != ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixListener;
    use std::thread;

    fn pipe() -> (File, File) {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
    }

    #[test]
    fn the_state_comes_back_as_it_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let state = State {
            from: "0.3.0".into(),
            listener: 3,
            asking: vec![4],
            waiting: vec![5],
            sessions: Vec::new(),
            flows: Vec::new(),
            removals: Vec::new(),
            moves: Vec::new(),
            project_tokens: HashMap::new(),
        };
        let fd = write(dir.path(), &state).unwrap();
        // Unlinked: nothing is left on disk.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        let back = read(fd.as_raw_fd()).unwrap();
        std::mem::forget(fd);
        assert_eq!(back.from, "0.3.0");
        assert_eq!(
            (back.listener, back.asking, back.waiting),
            (3, vec![4], vec![5])
        );
    }

    #[test]
    fn a_crystal_that_reads_another_kind_of_handover_is_refused() {
        let exe = std::env::current_exe().unwrap();
        assert!(check(&exe, FORMAT).is_ok());
        assert!(check(&exe, FORMAT + 1).is_err());
        assert!(check(Path::new("/no/such/crystal"), FORMAT).is_err());
    }

    #[test]
    fn a_descriptor_kept_across_exec_is_inherited_back() {
        let (read_end, _write_end) = pipe();
        let number = keep_across_exec(read_end.as_fd()).unwrap();
        let flags = unsafe { libc::fcntl(number, libc::F_GETFD) };
        assert_eq!(flags & libc::FD_CLOEXEC, 0);
        std::mem::forget(read_end);
        let inherited = inherit(number).unwrap();
        let flags = unsafe { libc::fcntl(inherited.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    #[test]
    fn readers_read_until_they_are_stopped_and_leave_the_rest() {
        let readers = Readers::new().unwrap();
        let (source, mut sink) = pipe();
        let mut buf = [0; 16];
        sink.write_all(b"one").unwrap();
        assert_eq!(readers.read(&source, &mut buf).unwrap(), Got::Bytes(3));
        sink.write_all(b"two").unwrap();
        readers.stop();
        assert_eq!(readers.read(&source, &mut buf).unwrap(), Got::Stopped);
        // What wasn't read is still there for whoever reads next.
        assert_eq!((&source).read(&mut buf).unwrap(), 3);
        assert_eq!(&buf[..3], b"two");
    }

    #[test]
    fn a_reader_waiting_on_a_terminal_stops_too() {
        let (mut master, mut slave) = (0, 0);
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0);
        let (master, mut slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
        let readers = Arc::new(Readers::new().unwrap());
        slave.write_all(b"hi").unwrap();
        let mut buf = [0; 16];
        assert!(matches!(
            readers.read(&master, &mut buf).unwrap(),
            Got::Bytes(_)
        ));
        let reading = thread::spawn({
            let readers = readers.clone();
            move || readers.read(&master, &mut [0; 16]).unwrap()
        });
        readers.stop();
        assert_eq!(reading.join().unwrap(), Got::Stopped);
    }

    #[test]
    fn a_reader_sees_the_end() {
        let readers = Readers::new().unwrap();
        let (source, sink) = pipe();
        drop(sink);
        assert_eq!(readers.read(&source, &mut [0; 4]).unwrap(), Got::End);
    }

    #[test]
    fn the_gate_waits_for_answers_then_keeps_newcomers_for_the_next_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("test.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let gate = Arc::new(Gate::default());

        let first = UnixStream::connect(&socket).unwrap();
        let (_conn, ticket) = gate.admit(listener.accept().unwrap().0).unwrap();
        drop(first);
        // The daemon's accepting loop, taking connections until the gate
        // keeps one.
        let accepting = thread::spawn({
            let gate = gate.clone();
            move || {
                for conn in listener.incoming() {
                    if gate.admit(conn.unwrap()).is_none() {
                        return;
                    }
                }
            }
        });
        // Answered a moment later, well after the gate would otherwise have
        // closed.
        let answered = Arc::new(AtomicBool::new(false));
        let answering = thread::spawn({
            let answered = answered.clone();
            move || {
                thread::sleep(Duration::from_millis(100));
                answered.store(true, Ordering::SeqCst);
                drop(ticket);
            }
        });
        let waiting = gate.close(&socket, Instant::now() + Duration::from_secs(5));
        assert!(answered.load(Ordering::SeqCst), "it waited for the answer");
        assert_eq!(waiting.len(), 1, "the connection that woke it");
        answering.join().unwrap();
        accepting.join().unwrap();
    }

    #[test]
    fn a_helper_is_waited_for_then_stopped() {
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        static OURS: Helpers = Helpers::new();
        let helper = OURS.started(child.id());
        let reaping = thread::spawn(move || {
            child.wait().unwrap();
            drop(helper);
        });
        let started = Instant::now();
        OURS.finish(Instant::now() + Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(5));
        reaping.join().unwrap();
        assert!(OURS.running.lock().unwrap().is_empty());
    }
}
