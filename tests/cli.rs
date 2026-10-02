//! Drives the real binary against a daemon of its own per test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

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

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_crystal"))
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .current_dir(self.dir.path())
            .output()
            .unwrap()
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
