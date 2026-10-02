//! Drives the real binary against a daemon of its own per test.

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const CRYSTAL: &str = env!("CARGO_BIN_EXE_crystal");

struct Crystal {
    dir: TempDir,
    socket: PathBuf,
}

impl Crystal {
    fn new() -> Crystal {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crystal.sock");
        Crystal { dir, socket }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(CRYSTAL);
        command
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .current_dir(self.dir.path());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// Runs a command that must succeed, and returns what it printed.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "crystal {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Runs a command that must fail, and returns its error.
    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(!out.status.success(), "crystal {args:?} succeeded");
        String::from_utf8(out.stderr).unwrap()
    }

    /// The `ls` row for `name`, split into its columns.
    fn row(&self, name: &str) -> Option<Vec<String>> {
        self.ok(&["ls"])
            .lines()
            .skip(1)
            .map(|line| line.split("  ").filter(|cell| !cell.is_empty()))
            .map(|cells| {
                cells
                    .map(|cell| cell.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .find(|cells| cells[0] == name)
    }

    fn pid(&self, name: &str) -> i32 {
        self.row(name).unwrap()[2].parse().unwrap()
    }

    /// Runs a crystal command that attaches, and waits until it has taken
    /// over its terminal: keys typed before that would go to the terminal,
    /// not the session.
    fn attach(&self, args: &[&str]) -> Terminal {
        let terminal = self.terminal(args);
        eventually("crystal has attached", || {
            terminal.screen.lock().unwrap().screen().alternate_screen()
        });
        terminal
    }

    /// Runs crystal in a terminal of its own, the way a person would.
    fn terminal(&self, args: &[&str]) -> Terminal {
        let pty = native_pty_system().openpty(size(24, 80)).unwrap();
        let mut command = CommandBuilder::new(CRYSTAL);
        command.arg("--socket");
        command.arg(&self.socket);
        command.args(args);
        command.cwd(self.dir.path());
        command.env_remove("CRYSTAL_SESSION");
        let child = pty.slave.spawn_command(command).unwrap();
        drop(pty.slave);

        let screen = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        let mut output = pty.master.try_clone_reader().unwrap();
        thread::spawn({
            let screen = screen.clone();
            move || {
                let mut buf = [0; 4096];
                while let Ok(n @ 1..) = output.read(&mut buf) {
                    screen.lock().unwrap().process(&buf[..n]);
                }
            }
        });
        Terminal {
            screen,
            keys: pty.master.take_writer().unwrap(),
            pty: pty.master,
            child,
        }
    }
}

struct Terminal {
    screen: Arc<Mutex<vt100::Parser>>,
    keys: Box<dyn Write + Send>,
    pty: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Terminal {
    fn text(&self) -> String {
        self.screen.lock().unwrap().screen().contents()
    }

    fn shows(&self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.text().contains(text) {
            assert!(
                Instant::now() < deadline,
                "{text:?} never showed up; the screen was:\n{}",
                self.text()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn type_keys(&mut self, keys: &str) {
        self.keys.write_all(keys.as_bytes()).unwrap();
        self.keys.flush().unwrap();
    }

    fn resize(&self, rows: u16, cols: u16) {
        self.pty.resize(size(rows, cols)).unwrap();
        self.screen
            .lock()
            .unwrap()
            .screen_mut()
            .set_size(rows, cols);
    }

    /// Waits for crystal to exit, and says whether it succeeded.
    fn exit(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.success();
            }
            assert!(Instant::now() < deadline, "crystal never exited");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
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

/// Waits for a file a session writes, ended by a newline.
fn written(file: &Path) -> String {
    eventually(&format!("{} is written", file.display()), || {
        std::fs::read_to_string(file).is_ok_and(|text| text.ends_with('\n'))
    });
    std::fs::read_to_string(file).unwrap()
}

impl Drop for Crystal {
    fn drop(&mut self) {
        let _ = self.run(&["kill-server"]);
    }
}

fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn new_starts_the_daemon_and_the_session_shows_in_ls() {
    let crystal = Crystal::new();
    assert_eq!(
        crystal.ok(&["new", "-n", "agent", "sleep", "30"]),
        "agent\n"
    );

    let row = crystal.row("agent").unwrap();
    assert_eq!(row[1], "running");
    assert_eq!(row[4], "sleep 30");
    assert!(alive(crystal.pid("agent")));
}

#[test]
fn names_come_from_the_program_and_never_repeat() {
    let crystal = Crystal::new();
    assert_eq!(crystal.ok(&["new", "sleep", "30"]), "sleep\n");
    assert_eq!(crystal.ok(&["new", "sleep", "30"]), "sleep-2\n");
    assert!(
        crystal
            .fails(&["new", "-n", "sleep", "sleep", "30"])
            .contains("already exists")
    );
}

#[test]
fn ls_shows_how_a_session_ended() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "fails", "--", "sh", "-c", "exit 3"]);
    crystal.ok(&["new", "-n", "killed", "sh", "-c", "kill -TERM $$"]);

    eventually("both have ended", || {
        crystal.row("fails").unwrap()[1] == "exited 3"
            && crystal.row("killed").unwrap()[1] == "killed (Terminated)"
    });
    assert_eq!(crystal.row("fails").unwrap()[4], "sh -c 'exit 3'");
}

#[test]
fn a_session_runs_where_it_was_asked_to() {
    let crystal = Crystal::new();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    crystal.ok(&["new", "-c", cwd, "sh", "-c", "pwd > where"]);

    let file = dir.path().join("where");
    eventually("the session has written its directory", || {
        std::fs::read_to_string(&file).is_ok_and(|dir| dir.ends_with('\n'))
    });
    let written = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        Path::new(written.trim()).canonicalize().unwrap(),
        dir.path().canonicalize().unwrap()
    );
}

#[test]
fn a_session_knows_its_name_and_its_daemon() {
    let crystal = Crystal::new();
    crystal.ok(&[
        "new",
        "-n",
        "probe",
        "sh",
        "-c",
        "echo $CRYSTAL_SESSION $CRYSTAL_SOCKET $TERM > env",
    ]);

    let file = crystal.dir.path().join("env");
    eventually("the session has written its env", || {
        std::fs::read_to_string(&file).is_ok_and(|env| env.ends_with('\n'))
    });
    let env = std::fs::read_to_string(&file).unwrap();
    let socket = crystal.socket.to_str().unwrap();
    assert_eq!(env.trim(), format!("probe {socket} xterm-256color"));
}

#[test]
fn kill_stops_the_program_and_drops_the_row() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "doomed", "sleep", "30"]);
    let pid = crystal.pid("doomed");

    crystal.ok(&["kill", "doomed"]);
    assert!(crystal.row("doomed").is_none());
    eventually("the program has exited", || !alive(pid));
    assert!(
        crystal
            .fails(&["kill", "doomed"])
            .contains("no session named doomed")
    );
}

#[test]
fn kill_falls_back_to_sigkill_when_the_hang_up_is_ignored() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "stubborn", "sh", "-c", "trap '' HUP; sleep 30"]);
    let pid = crystal.pid("stubborn");

    crystal.ok(&["kill", "stubborn"]);
    thread::sleep(Duration::from_millis(500));
    assert!(alive(pid), "the hang-up alone shouldn't have stopped it");
    eventually("the program has been killed", || !alive(pid));
}

#[test]
fn kill_server_stops_every_session_and_the_daemon() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "one", "sleep", "30"]);
    crystal.ok(&["new", "-n", "two", "sleep", "30"]);
    let pids = [crystal.pid("one"), crystal.pid("two")];

    crystal.ok(&["kill-server"]);
    assert!(pids.iter().all(|&pid| !alive(pid)));
    assert!(!crystal.socket.exists());
    assert!(
        crystal
            .fails(&["kill-server"])
            .contains("no daemon is running")
    );
}

#[test]
fn ls_without_a_daemon_prints_nothing_and_starts_nothing() {
    let crystal = Crystal::new();
    assert_eq!(crystal.ok(&["ls"]), "");
    assert!(!crystal.socket.exists());
}

#[test]
fn new_refuses_a_program_that_does_not_exist() {
    let crystal = Crystal::new();
    let err = crystal.fails(&["new", "no-such-program-for-crystal"]);
    assert_eq!(
        err,
        "crystal: command not found: no-such-program-for-crystal\n"
    );
}

#[test]
fn a_stale_socket_does_not_stop_the_daemon_from_starting() {
    let crystal = Crystal::new();
    let listener = std::os::unix::net::UnixListener::bind(&crystal.socket).unwrap();
    drop(listener);
    assert!(crystal.socket.exists());

    assert_eq!(
        crystal.ok(&["new", "-n", "fresh", "sleep", "30"]),
        "fresh\n"
    );
}

#[test]
fn attach_shows_the_session_until_ctrl_backslash() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "cat", "cat"]);

    let mut terminal = crystal.attach(&["attach", "cat"]);
    terminal.type_keys("hello from the keyboard\r");
    terminal.shows("hello from the keyboard");

    terminal.type_keys("\x1c");
    terminal.shows("[detached from cat]");
    assert!(terminal.exit());
    assert_eq!(crystal.row("cat").unwrap()[1], "running");
}

#[test]
fn attach_starts_from_what_is_already_on_screen() {
    let crystal = Crystal::new();
    crystal.ok(&[
        "new",
        "-n",
        "greeter",
        "sh",
        "-c",
        "echo ready when you are; echo > printed; sleep 30",
    ]);
    written(&crystal.dir.path().join("printed"));

    let terminal = crystal.terminal(&["attach", "greeter"]);
    terminal.shows("ready when you are");
}

#[test]
fn new_attaches_when_run_in_a_terminal() {
    let crystal = Crystal::new();
    let mut terminal = crystal.attach(&["new", "-n", "shell", "sh"]);
    terminal.type_keys("echo I am $CRYSTAL_SESSION\r");
    terminal.shows("I am shell");

    terminal.type_keys("\x1c");
    terminal.shows("[detached from shell]");
    assert!(terminal.exit());
}

#[test]
fn attach_without_a_name_picks_the_newest_session() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "older", "sleep", "30"]);
    crystal.ok(&["new", "-n", "newest", "sh"]);

    let mut terminal = crystal.attach(&["attach"]);
    terminal.type_keys("echo I am $CRYSTAL_SESSION\r");
    terminal.shows("I am newest");
}

#[test]
fn attach_ends_when_the_program_does() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "brief", "sh", "-c", "read line; exit 7"]);

    let mut terminal = crystal.attach(&["attach", "brief"]);
    terminal.type_keys("bye\r");
    terminal.shows("[brief exited 7]");
    assert!(terminal.exit());
}

#[test]
fn attaching_to_an_ended_session_prints_its_last_screen() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "done", "sh", "-c", "echo last words"]);
    eventually("the session has ended", || {
        crystal.row("done").unwrap()[1] == "exited 0"
    });

    let mut terminal = crystal.terminal(&["attach", "done"]);
    terminal.shows("last words");
    terminal.shows("[done exited 0]");
    assert!(terminal.exit());
}

#[test]
fn a_resize_reaches_the_program() {
    let crystal = Crystal::new();
    crystal.ok(&[
        "new",
        "-n",
        "sizer",
        "sh",
        "-c",
        "trap 'stty size > size' WINCH; echo watching; while :; do sleep 0.05; done",
    ]);

    let terminal = crystal.attach(&["attach", "sizer"]);
    terminal.shows("watching");
    terminal.resize(30, 100);
    assert_eq!(written(&crystal.dir.path().join("size")), "30 100\n");
}

#[test]
fn the_daemon_tells_a_program_where_its_cursor_is() {
    let crystal = Crystal::new();
    crystal.ok(&[
        "new",
        "-n",
        "asker",
        "sh",
        "-c",
        r"stty raw -echo; printf 'hi\033[6n'; dd bs=1 count=6 2>/dev/null > reply; echo >> reply",
    ]);

    assert_eq!(written(&crystal.dir.path().join("reply")), "\x1b[1;3R\n");
}

#[test]
fn attach_needs_a_terminal() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "x", "sleep", "30"]);
    assert!(
        crystal
            .fails(&["attach", "x"])
            .contains("attach needs a terminal")
    );
}

#[test]
fn attach_to_a_missing_session_fails() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "x", "sleep", "30"]);

    let mut terminal = crystal.terminal(&["attach", "nope"]);
    terminal.shows("no session named nope");
    assert!(!terminal.exit());
}

#[test]
fn a_session_cannot_attach_to_itself() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "loop", "sh"]);

    let mut terminal = crystal.attach(&["attach", "loop"]);
    terminal.type_keys(&format!("{CRYSTAL} attach loop\r"));
    terminal.shows("can't attach loop to itself");
}

#[test]
fn a_session_starts_from_the_environment_of_the_new_that_asked_for_it() {
    let crystal = Crystal::new();
    // The first `new` starts the daemon with its environment…
    crystal.ok(&["new", "-n", "first", "sleep", "30"]);
    // …but the second session gets the second `new`'s, minus the marks
    // of the agent it ran in.
    let out = crystal
        .command(&[
            "new",
            "-n",
            "second",
            "sh",
            "-c",
            "echo $GREETING ${CLAUDECODE:-unmarked} > env",
        ])
        .env("GREETING", "hello")
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    assert!(out.status.success());

    assert_eq!(written(&crystal.dir.path().join("env")), "hello unmarked\n");
}
