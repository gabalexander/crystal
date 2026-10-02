//! Drives the real binary against a daemon of its own per test.

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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
            .current_dir(self.dir.path())
            .envs(PLAIN_GIT);
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

    /// Opens the TUI and waits until it has taken over its terminal.
    fn tui(&self) -> Terminal {
        self.attach(&[])
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
        // So that a shell crystal starts is the same everywhere.
        command.env("SHELL", "/bin/sh");
        for (key, value) in PLAIN_GIT {
            command.env(key, value);
        }
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

/// Keeps the machine's own git config, like signed commits or hooks, out of
/// the git that tests and crystal run.
const PLAIN_GIT: [(&str, &str); 2] = [
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
];

/// Runs git in `dir` the way the tests need it, failing the test if git
/// fails, and returns what it printed.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=crystal",
            "-c",
            "user.email=crystal@example.com",
        ])
        .args(args)
        .envs(PLAIN_GIT)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Makes a git repository called `name` in `dir`, with one commit on
/// `main`.
fn git_repo(dir: &Path, name: &str) -> PathBuf {
    let repo = dir.join(name);
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    repo
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
    assert_eq!(row[6], "sleep 30");
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
    assert_eq!(crystal.row("fails").unwrap()[6], "sh -c 'exit 3'");
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
    // A daemon killed outright leaves its socket behind. The socket is made
    // in a process of its own: one made in this test process could leak into
    // a program another test starts at that moment, and go on listening.
    let mut daemon = crystal
        .command(&["daemon"])
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    eventually("the daemon is listening", || crystal.socket.exists());
    daemon.kill().unwrap();
    daemon.wait().unwrap();
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

#[test]
fn the_tui_lists_the_sessions_and_shows_the_selected_one() {
    let crystal = Crystal::new();
    for name in ["alpha", "beta"] {
        let script = format!("echo {name} is here; echo > {name}-ready; sleep 30");
        crystal.ok(&["new", "-n", name, "sh", "-c", &script]);
        written(&crystal.dir.path().join(format!("{name}-ready")));
    }

    let mut tui = crystal.tui();
    tui.shows("▶ alpha");
    tui.shows("▶ beta");
    tui.shows("alpha is here");

    tui.type_keys("j");
    tui.shows("beta is here");
}

#[test]
fn keys_go_to_the_pane_after_enter_and_back_to_the_list_after_ctrl_backslash() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "cat", "cat"]);

    let mut tui = crystal.tui();
    tui.shows("▶ cat");
    tui.type_keys("\r");
    tui.shows("typing into the session");
    tui.type_keys("hello pane\r");
    tui.shows("hello pane");

    tui.type_keys("\x1c");
    tui.shows("q quit");
    // Back on the list, q quits rather than going to cat.
    tui.type_keys("q");
    assert!(tui.exit());
}

#[test]
fn n_starts_a_shell_and_hands_it_the_keyboard() {
    let crystal = Crystal::new();
    let mut tui = crystal.tui();
    assert!(crystal.socket.exists(), "the TUI starts the daemon");
    tui.shows("No sessions yet");

    tui.type_keys("n");
    tui.shows("▶ sh");
    tui.shows("typing into the session");
    tui.type_keys("echo I am $CRYSTAL_SESSION\r");
    tui.shows("I am sh");
}

#[test]
fn x_kills_the_selected_session() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "doomed", "sleep", "30"]);
    let pid = crystal.pid("doomed");

    let mut tui = crystal.tui();
    tui.shows("▶ doomed");
    tui.type_keys("x");
    tui.shows("No sessions yet");
    eventually("the program has exited", || !alive(pid));
}

#[test]
fn q_quits_the_tui_and_the_sessions_keep_running() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "stays", "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("▶ stays");
    tui.type_keys("q");
    assert!(tui.exit());
    assert_eq!(crystal.row("stays").unwrap()[1], "running");
}

#[test]
fn an_ended_session_shows_how_it_ended() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "done", "sh", "-c", "echo last words; exit 3"]);
    eventually("the session has ended", || {
        crystal.row("done").unwrap()[1] == "exited 3"
    });

    let tui = crystal.tui();
    tui.shows("■ done exited 3");
    tui.shows("last words");
}

#[test]
fn the_session_in_the_pane_is_sized_to_the_pane() {
    let crystal = Crystal::new();
    crystal.ok(&[
        "new",
        "-n",
        "sizer",
        "sh",
        "-c",
        "trap 'stty size > size' WINCH; echo watching; while :; do sleep 0.05; done",
    ]);
    let size = crystal.dir.path().join("size");
    let size_is = |expected: &str| std::fs::read_to_string(&size).is_ok_and(|s| s == expected);

    // The terminal is 24 by 80; inside the pane's border, beside the
    // sidebar and above the footer, that leaves 21 by 50.
    let tui = crystal.tui();
    tui.shows("watching");
    eventually("the session is the pane's size", || size_is("21 50\n"));

    tui.resize(30, 100);
    eventually("the session follows the pane", || size_is("27 70\n"));
}

#[test]
fn the_tui_needs_a_terminal() {
    let crystal = Crystal::new();
    assert!(crystal.fails(&[]).contains("crystal needs a terminal"));
    assert!(!crystal.socket.exists());
}

/// A stand-in for Claude Code: a `claude` that writes down the arguments
/// it was started with, one per line, and waits. Returns the directory to
/// put on the PATH.
fn fake_claude(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > args\nsleep 30\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&claude).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&claude, permissions).unwrap();
    bin
}

/// Runs a hook command the way Claude Code does: through a shell, in the
/// session's environment, with the event on stdin. A hook must succeed and
/// print nothing.
fn run_hook(crystal: &Crystal, session: &str, hook: &str, event: &str) {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(hook)
        .env("CRYSTAL_SESSION", session)
        .env("CRYSTAL_SOCKET", &crystal.socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(event.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "the hook failed for {event}");
    assert!(out.stdout.is_empty(), "the hook printed for {event}");
}

#[test]
fn claude_reports_what_it_is_doing_through_its_hooks() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = crystal
        .command(&["new", "-n", "agent", "claude", "--resume"])
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(crystal.row("agent").unwrap()[6], "claude --resume");

    // crystal added its hooks ahead of the arguments it was given.
    let args = written(&crystal.dir.path().join("args"));
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(args[0], "--settings");
    assert_eq!(args[2], "--resume");
    let settings: serde_json::Value = serde_json::from_str(args[1]).unwrap();
    let hook = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();

    let steps = [
        (
            r#"{"hook_event_name":"SessionStart","source":"startup"}"#,
            "idle",
        ),
        (r#"{"hook_event_name":"UserPromptSubmit"}"#, "working"),
        (r#"{"hook_event_name":"PermissionRequest"}"#, "waiting"),
        (r#"{"hook_event_name":"PostToolUse"}"#, "working"),
        (r#"{"hook_event_name":"Stop"}"#, "done"),
    ];
    for (event, status) in steps {
        run_hook(&crystal, "agent", hook, event);
        assert_eq!(crystal.row("agent").unwrap()[1], status, "after {event}");
    }

    // Looking at a finished turn marks it seen.
    let mut terminal = crystal.attach(&["attach", "agent"]);
    terminal.type_keys("\x1c");
    assert!(terminal.exit());
    assert_eq!(crystal.row("agent").unwrap()[1], "idle");
}

#[test]
fn a_hook_outside_a_session_does_nothing_quietly() {
    let crystal = Crystal::new();
    let mut child = crystal
        .command(&["hook", "claude"])
        .env_remove("CRYSTAL_SESSION")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"hook_event_name":"Stop"}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!crystal.socket.exists(), "a hook never starts the daemon");
}

#[test]
fn the_screen_says_what_an_agent_without_hooks_is_doing() {
    let crystal = Crystal::new();
    // A pretend agent that draws what agents draw, a stage at a time,
    // moving on when the test creates the stage's file: a spinner in the
    // title while it works, then a question, then its prompt again.
    let script = r#"
        wait_for() { while [ ! -e "$1" ]; do sleep 0.05; done; }
        printf '> '
        wait_for work; printf '\033]0;⠋ Thinking\007'
        wait_for ask; printf '\033]0;\007\r\033[2KDo you want to proceed?'
        wait_for rest; printf '\r\033[2K> '
        sleep 30
    "#;
    crystal.ok(&["new", "-n", "agent", "sh", "-c", script]);
    let status = || crystal.row("agent").unwrap()[1].clone();
    assert_eq!(status(), "running");

    for (stage, expected) in [("work", "working"), ("ask", "waiting"), ("rest", "done")] {
        std::fs::write(crystal.dir.path().join(stage), "").unwrap();
        eventually(&format!("the agent is {expected}"), || status() == expected);
    }
}

/// The line number of the first line of `text` that holds `needle`.
fn line_with(text: &str, needle: &str) -> usize {
    let found = text.lines().position(|line| line.contains(needle));
    found.unwrap_or_else(|| panic!("{needle:?} isn't on screen:\n{text}"))
}

#[test]
fn ls_shows_the_project_and_branch_of_each_session() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "inside", "-c", repo_arg, "sleep", "30"]);
    crystal.ok(&["new", "-n", "outside", "sleep", "30"]);

    let inside = crystal.row("inside").unwrap();
    assert_eq!((inside[3].as_str(), inside[4].as_str()), ("app", "main"));
    let outside = crystal.row("outside").unwrap();
    assert_eq!((outside[3].as_str(), outside[4].as_str()), ("-", "-"));

    // A switch of branch shows straight away.
    git(&repo, &["switch", "-q", "-c", "other"]);
    assert_eq!(crystal.row("inside").unwrap()[4], "other");
}

#[test]
fn new_with_a_worktree_starts_the_session_on_a_new_branch_beside_the_repo() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    crystal.ok(&[
        "new",
        "-n",
        "fixer",
        "-c",
        repo.to_str().unwrap(),
        "-w",
        "fix/typo",
        "sh",
        "-c",
        "pwd > where; sleep 30",
    ]);

    let worktree = crystal.dir.path().join("app.worktrees/fix-typo");
    let started_in = written(&worktree.join("where"));
    assert_eq!(
        Path::new(started_in.trim()).canonicalize().unwrap(),
        worktree.canonicalize().unwrap()
    );
    let row = crystal.row("fixer").unwrap();
    assert_eq!((row[3].as_str(), row[4].as_str()), ("app", "fix/typo"));
    assert!(git(&repo, &["branch", "--list", "fix/typo"]).contains("fix/typo"));
}

#[test]
fn new_with_a_branch_that_exists_checks_it_out() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    git(&repo, &["branch", "older"]);
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&[
        "new", "-n", "x", "-c", repo_arg, "-w", "older", "sleep", "30",
    ]);

    assert!(crystal.dir.path().join("app.worktrees/older").is_dir());
    assert_eq!(crystal.row("x").unwrap()[4], "older");
}

#[test]
fn new_with_a_worktree_needs_a_repository() {
    let crystal = Crystal::new();
    let err = crystal.fails(&["new", "-w", "feat", "sleep", "30"]);
    assert!(err.contains("isn't in a git repository"), "{err}");
    assert!(!crystal.socket.exists(), "nothing was started");
}

#[test]
fn what_git_refuses_is_said_in_gits_words() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    let err = crystal.fails(&["new", "-c", repo_arg, "-w", "bad..name", "sleep", "30"]);
    assert!(err.contains("not a valid branch name"), "{err}");
}

#[test]
fn worktree_rm_waits_until_no_session_runs_in_it() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&[
        "new", "-n", "fixer", "-c", repo_arg, "-w", "fix", "sleep", "30",
    ]);
    let worktree = crystal.dir.path().join("app.worktrees/fix");

    // Named by its branch, from inside the repository.
    let out = crystal
        .command(&["worktree", "rm", "fix"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("fixer still running in"), "{err}");
    assert!(worktree.is_dir());

    // Named by its directory, once its session is gone.
    crystal.ok(&["kill", "fixer"]);
    crystal.ok(&["worktree", "rm", "app.worktrees/fix"]);
    assert!(!worktree.exists());
}

#[test]
fn worktree_rm_keeps_work_that_isnt_committed() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "x", "-c", repo_arg, "-w", "fix", "true"]);
    let worktree = crystal.dir.path().join("app.worktrees/fix");
    std::fs::write(worktree.join("notes.txt"), "half done\n").unwrap();

    let err = crystal.fails(&["worktree", "rm", "app.worktrees/fix"]);
    assert!(err.contains("untracked"), "{err}");
    assert!(worktree.join("notes.txt").exists());
}

#[test]
fn the_tui_groups_sessions_by_project_then_worktree() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&[
        "new", "-n", "fixer", "-c", repo_arg, "-w", "fix", "sleep", "30",
    ]);
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);
    crystal.ok(&["new", "-n", "shell", "sleep", "30"]);

    let tui = crystal.tui();
    tui.shows("▶ shell");
    let text = tui.text();
    let order = [
        line_with(&text, "│app"),
        line_with(&text, "⌂ main"),
        line_with(&text, "▶ planner"),
        line_with(&text, "⎇ fix"),
        line_with(&text, "▶ fixer"),
        line_with(&text, "outside git"),
        line_with(&text, "▶ shell"),
    ];
    assert!(order.is_sorted(), "out of order: {order:?}\n{text}");
}

#[test]
fn w_in_the_tui_starts_a_shell_in_a_new_worktree() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    crystal.ok(&[
        "new",
        "-n",
        "planner",
        "-c",
        repo.to_str().unwrap(),
        "sleep",
        "30",
    ]);

    let mut tui = crystal.tui();
    tui.shows("▶ planner");
    tui.type_keys("w");
    tui.shows("branch for the new worktree:");
    tui.type_keys("spike\r");
    tui.shows("⎇ spike");
    tui.shows("typing into the session");
    assert!(crystal.dir.path().join("app.worktrees/spike").is_dir());
}

impl Crystal {
    /// Starts the daemon in a process of this test's own, so the test can
    /// kill it the way a crash would.
    fn start_daemon(&self) -> std::process::Child {
        let daemon = self
            .command(&["daemon"])
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        eventually("the daemon is listening", || self.socket.exists());
        daemon
    }

    /// The sessions the daemon has written down, as JSON.
    fn saved(&self) -> String {
        std::fs::read_to_string(self.socket.with_extension("sessions.json")).unwrap_or_default()
    }
}

fn crash(mut daemon: std::process::Child) {
    daemon.kill().unwrap();
    daemon.wait().unwrap();
}

#[test]
fn running_sessions_come_back_after_the_daemon_dies() {
    let crystal = Crystal::new();
    let daemon = crystal.start_daemon();
    crystal.ok(&["new", "-n", "keeper", "sleep", "300"]);
    crystal.ok(&["new", "-n", "finished", "sh", "-c", "exit 0"]);
    eventually("only the running session is saved", || {
        let saved = crystal.saved();
        saved.contains("keeper") && !saved.contains("finished")
    });
    let old_pid = crystal.pid("keeper");

    crash(daemon);
    eventually("the old program has gone with its terminal", || {
        !alive(old_pid)
    });

    // The next command starts a new daemon, which starts keeper again.
    crystal.ok(&["new", "-n", "fresh", "sleep", "300"]);
    let row = crystal.row("keeper").unwrap();
    assert_eq!(row[1], "running");
    assert_ne!(crystal.pid("keeper"), old_pid);
    assert!(crystal.row("finished").is_none());
}

#[test]
fn kill_server_means_the_sessions_stay_stopped() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "gone", "sleep", "300"]);
    eventually("the session is saved", || crystal.saved().contains("gone"));

    crystal.ok(&["kill-server"]);
    assert_eq!(crystal.saved(), "");
    crystal.ok(&["new", "-n", "next", "sleep", "300"]);
    assert!(crystal.row("gone").is_none());
}

#[test]
fn claude_picks_its_conversation_up_again_after_a_restart() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let daemon = crystal.start_daemon();
    let out = crystal
        .command(&["new", "-n", "agent", "claude", "--continue"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());

    // Claude's hooks name its conversation, whose transcript exists once a
    // prompt has been sent.
    let args = written(&crystal.dir.path().join("args"));
    let settings: serde_json::Value = serde_json::from_str(args.lines().nth(1).unwrap()).unwrap();
    let hook = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let transcript = crystal.dir.path().join("abc-123.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    let event = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "abc-123",
        "transcript_path": transcript,
    });
    run_hook(&crystal, "agent", hook, &event.to_string());
    eventually("the conversation is saved", || {
        crystal.saved().contains("abc-123")
    });

    crash(daemon);
    std::fs::remove_file(crystal.dir.path().join("args")).unwrap();
    let out = crystal
        .command(&["new", "-n", "other", "sleep", "300"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());

    // Started again with crystal's resume in place of its own --continue.
    let args = written(&crystal.dir.path().join("args"));
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(args[2..], ["--resume", "abc-123"]);
}

/// Waits until the session's screen shows `text`, as `crystal read` sees it.
fn shows_on_screen(crystal: &Crystal, name: &str, text: &str) {
    eventually(&format!("{name} shows {text:?}"), || {
        crystal.ok(&["read", name]).contains(text)
    });
}

#[test]
fn send_types_into_a_program_and_presses_enter() {
    let crystal = Crystal::new();
    let script = r#"read line; echo "got $line" > got; sleep 30"#;
    crystal.ok(&["new", "-n", "reader", "sh", "-c", script]);

    crystal.ok(&["send", "reader", "hello", "there"]);
    assert_eq!(
        written(&crystal.dir.path().join("got")),
        "got hello there\n"
    );
}

#[test]
fn send_marks_the_text_as_a_paste_for_a_program_that_asks() {
    let crystal = Crystal::new();
    // A program that asks for bracketed paste, says it's ready, and keeps
    // the first 21 bytes it gets: the marked text, then the Enter.
    let script = r"stty raw -echo; printf '\033[?2004hready'; head -c 21 > received; echo >> received; sleep 30";
    crystal.ok(&["new", "-n", "pasty", "sh", "-c", script]);
    shows_on_screen(&crystal, "pasty", "ready");

    crystal.ok(&["send", "pasty", "hi there"]);
    assert_eq!(
        written(&crystal.dir.path().join("received")),
        "\x1b[200~hi there\x1b[201~\r\n"
    );
}

#[test]
fn send_no_enter_types_and_leaves_it_there() {
    let crystal = Crystal::new();
    let script = r"stty raw -echo; printf ready; head -c 6 > received; echo >> received; sleep 30";
    crystal.ok(&["new", "-n", "typist", "sh", "-c", script]);
    shows_on_screen(&crystal, "typist", "ready");

    // With an Enter after the first, the program would get `hello\r`.
    crystal.ok(&["send", "--no-enter", "typist", "hello"]);
    crystal.ok(&["send", "--no-enter", "typist", "!"]);
    assert_eq!(written(&crystal.dir.path().join("received")), "hello!\n");
}

#[test]
fn read_prints_the_screen_and_lines_keeps_the_last_rows() {
    let crystal = Crystal::new();
    let script = r"printf 'one\ntwo\n\nthree\n'; sleep 30";
    crystal.ok(&["new", "-n", "printer", "sh", "-c", script]);
    shows_on_screen(&crystal, "printer", "three");

    assert_eq!(crystal.ok(&["read", "printer"]), "one\ntwo\n\nthree\n");
    assert_eq!(
        crystal.ok(&["read", "printer", "--lines", "2"]),
        "two\nthree\n"
    );
}

#[test]
fn wait_returns_how_a_program_ended() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "brief", "sh", "-c", "read go; exit 4"]);

    // The program can't end before it's sent a line, so the wait must
    // still be waiting when the line goes.
    let waiting = crystal
        .command(&["wait", "brief"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    crystal.ok(&["send", "brief", "go"]);
    let out = waiting.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "exited 4\n");
}

#[test]
fn wait_returns_what_an_agent_settles_on() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);
    let hook = format!("'{CRYSTAL}' hook claude");
    run_hook(
        &crystal,
        "agent",
        &hook,
        r#"{"hook_event_name":"UserPromptSubmit"}"#,
    );

    let waiting = crystal
        .command(&["wait", "agent"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    run_hook(
        &crystal,
        "agent",
        &hook,
        r#"{"hook_event_name":"PermissionRequest"}"#,
    );
    let out = waiting.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "waiting\n");

    run_hook(&crystal, "agent", &hook, r#"{"hook_event_name":"Stop"}"#);
    assert_eq!(crystal.ok(&["wait", "agent"]), "done\n");
}

#[test]
fn wait_gives_up_after_its_timeout() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "forever", "sleep", "30"]);

    let err = crystal.fails(&["wait", "forever", "--timeout", "0.3"]);
    assert!(err.contains("forever was still busy after 0.3s"), "{err}");
}

#[test]
fn send_wait_waits_for_the_turn_it_started() {
    let crystal = Crystal::new();
    // A pretend agent without hooks: for each line it's sent, it shows a
    // spinner in its title while it works for a second, then answers.
    let script = r#"
        while read line; do
            printf '\033]0;⠋ working\007'
            sleep 1
            printf '\033]0;\007'
            echo "answer to $line"
        done
    "#;
    crystal.ok(&["new", "-n", "agent", "sh", "-c", script]);

    assert_eq!(crystal.ok(&["send", "agent", "first", "--wait"]), "done\n");
    assert!(crystal.ok(&["read", "agent"]).contains("answer to first"));

    // The agent is done with the first turn, which must not end the wait
    // for the second.
    assert_eq!(crystal.ok(&["send", "agent", "second", "--wait"]), "done\n");
    assert!(crystal.ok(&["read", "agent"]).contains("answer to second"));
}

#[test]
fn an_agent_can_read_another_from_inside_its_session() {
    let crystal = Crystal::new();
    let other = "echo hello from other; sleep 30";
    crystal.ok(&["new", "-n", "other", "sh", "-c", other]);
    shows_on_screen(&crystal, "other", "hello from other");

    // Inside a session, crystal finds its daemon through CRYSTAL_SOCKET.
    let driver = format!("'{CRYSTAL}' read other > seen; sleep 30");
    crystal.ok(&["new", "-n", "driver", "sh", "-c", &driver]);
    assert!(written(&crystal.dir.path().join("seen")).contains("hello from other"));
}

#[test]
fn send_wait_and_read_say_when_a_session_is_missing_or_ended() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "gone", "true"]);
    eventually("gone has ended", || {
        crystal.row("gone").unwrap()[1] == "exited 0"
    });

    for args in [
        &["send", "nope", "hi"][..],
        &["wait", "nope"],
        &["read", "nope"],
    ] {
        assert!(crystal.fails(args).contains("no session named nope"));
    }
    assert!(
        crystal
            .fails(&["send", "gone", "hi"])
            .contains("gone has ended")
    );
}
