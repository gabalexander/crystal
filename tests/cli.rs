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
        let crystal = Crystal { dir, socket };
        // A config of the test's own: the developer's can't change what
        // the test sees, and no test pops up a real notification.
        crystal.configure("notify = false\n");
        crystal
    }

    /// Where the test's config lives: its `XDG_CONFIG_HOME`.
    fn config_home(&self) -> PathBuf {
        self.dir.path().join("config")
    }

    fn config_file(&self) -> PathBuf {
        self.config_home().join("crystal/config.toml")
    }

    /// Writes the test's config file.
    fn configure(&self, toml: &str) {
        std::fs::create_dir_all(self.config_home().join("crystal")).unwrap();
        std::fs::write(self.config_file(), toml).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(CRYSTAL);
        command
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .current_dir(self.dir.path())
            .env("XDG_CONFIG_HOME", self.config_home())
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
        self.attach_with_env(args, &[])
    }

    fn attach_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Terminal {
        let terminal = self.terminal_with_env(args, env);
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
        self.terminal_with_env(args, &[])
    }

    /// Like [`Crystal::terminal`], with `env` added to its environment.
    fn terminal_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Terminal {
        let pty = native_pty_system().openpty(size(24, 80)).unwrap();
        let mut command = CommandBuilder::new(CRYSTAL);
        command.arg("--socket");
        command.arg(&self.socket);
        command.args(args);
        command.cwd(self.dir.path());
        command.env_remove("CRYSTAL_SESSION");
        command.env_remove("CRYSTAL_SESSION_ID");
        // So that a shell crystal starts is the same everywhere.
        command.env("SHELL", "/bin/sh");
        command.env("XDG_CONFIG_HOME", self.config_home());
        for (key, value) in PLAIN_GIT.iter().chain(env) {
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

    /// Whether crystal has asked this terminal to send it the mouse.
    fn sends_the_mouse(&self) -> bool {
        let mode = self.screen.lock().unwrap().screen().mouse_protocol_mode();
        mode != vt100::MouseProtocolMode::None
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

    /// Waits until `text` is no longer on screen.
    fn hides(&self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.text().contains(text) {
            assert!(
                Instant::now() < deadline,
                "{text:?} never went away; the screen was:\n{}",
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
fn n_with_an_empty_line_starts_a_shell_and_hands_it_the_keyboard() {
    let crystal = Crystal::new();
    let mut tui = crystal.tui();
    assert!(crystal.socket.exists(), "the TUI starts the daemon");
    tui.shows("No sessions yet");

    tui.type_keys("n");
    tui.shows("new session: claude");
    // Ctrl+U clears the line, and an empty line is the shell.
    tui.type_keys("\x15\r");
    tui.shows("▶ sh");
    tui.shows("typing into the session");
    tui.type_keys("echo I am $CRYSTAL_SESSION\r");
    tui.shows("I am sh");
}

#[test]
fn n_starts_claude_with_its_hooks_and_the_rest_of_the_line_as_its_prompt() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);

    tui.type_keys("n");
    tui.shows("new session: claude");
    tui.type_keys(" fix the login bug\r");
    tui.shows("▶ claude");

    let args = written(&crystal.dir.path().join("args"));
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(args.len(), 3, "{args:?}");
    assert_eq!(args[0], "--settings");
    assert_eq!(args[2], "fix the login bug");
}

#[test]
fn x_asks_first_and_only_y_kills_the_selected_session() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "doomed", "sleep", "30"]);
    let pid = crystal.pid("doomed");

    let mut tui = crystal.tui();
    tui.shows("▶ doomed");
    tui.type_keys("x");
    tui.shows("kill doomed? y/n");
    tui.type_keys("n");
    tui.shows("x kill");
    assert!(alive(pid));

    tui.type_keys("x");
    tui.shows("kill doomed? y/n");
    tui.type_keys("y");
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
    run_hook_with(crystal, &[("CRYSTAL_SESSION", session)], hook, event);
}

/// Like [`run_hook`], with the session's part of the environment as given.
fn run_hook_with(crystal: &Crystal, session_env: &[(&str, &str)], hook: &str, event: &str) {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(hook)
        .envs(session_env.iter().copied())
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
fn w_in_the_tui_starts_a_command_in_a_new_worktree() {
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
    tui.shows("new session: claude");
    tui.type_keys("\x15sh -c 'pwd > where; sleep 30'\r");
    tui.shows("⎇ spike");
    tui.shows("typing into the session");

    let worktree = crystal.dir.path().join("app.worktrees/spike");
    let written = written(&worktree.join("where"));
    assert_eq!(
        Path::new(written.trim()).canonicalize().unwrap(),
        worktree.canonicalize().unwrap()
    );
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

#[test]
fn s_splits_a_session_off_and_it_stays_while_the_selection_moves() {
    let crystal = Crystal::new();
    for name in ["alpha", "beta"] {
        let script = format!("echo {name} is here; echo > {name}-ready; sleep 30");
        crystal.ok(&["new", "-n", name, "sh", "-c", &script]);
        written(&crystal.dir.path().join(format!("{name}-ready")));
    }

    let mut tui = crystal.tui();
    tui.shows("alpha is here");
    tui.type_keys("s");
    tui.shows("alpha has a pane of its own");

    tui.type_keys("j");
    tui.shows("beta is here");
    tui.shows("alpha is here");

    // Back on alpha, s closes its split, and beta leaves the screen.
    tui.type_keys("k");
    tui.shows("alpha has a pane of its own");
    tui.type_keys("s");
    tui.hides("alpha has a pane of its own");
    tui.hides("beta is here");
    tui.shows("alpha is here");
}

#[test]
fn tab_takes_the_keyboard_on_to_a_split_and_its_session_gets_the_keys() {
    let crystal = Crystal::new();
    crystal.ok(&[
        "new",
        "-n",
        "reader",
        "sh",
        "-c",
        "read line; echo \"$line\" > got; sleep 30",
    ]);
    crystal.ok(&["new", "-n", "other", "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("▶ reader");
    tui.type_keys("s");
    tui.type_keys("j");

    // The first Tab goes to the selection's pane, the next to the split.
    tui.type_keys("\t");
    tui.shows("typing into the session");
    tui.type_keys("\x1c");
    tui.shows("q quit");
    tui.type_keys("\t");
    tui.shows("typing into the session");
    tui.type_keys("hello split\r");

    assert_eq!(written(&crystal.dir.path().join("got")), "hello split\n");
}

#[test]
fn each_pane_sizes_its_own_session() {
    let crystal = Crystal::new();
    for name in ["left", "right"] {
        let script = format!(
            "trap 'stty size > {name}-size' WINCH; echo {name} watching; \
             while :; do sleep 0.05; done"
        );
        crystal.ok(&["new", "-n", name, "sh", "-c", &script]);
    }
    let size_of = |name: &str| -> (u16, u16) {
        let file = crystal.dir.path().join(format!("{name}-size"));
        let size = std::fs::read_to_string(file).unwrap_or_default();
        let mut numbers = size.split_whitespace().map(|n| n.parse().unwrap());
        (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0))
    };

    let mut tui = crystal.tui();
    tui.shows("left watching");
    tui.type_keys("s");
    tui.type_keys("j");
    tui.shows("right watching");
    tui.shows("left watching");

    // At 24 by 80 there are 52 columns beside the sidebar, too few to share
    // side by side, so the panes are stacked: each 50 columns inside its
    // border, with the 23 rows above the footer shared between them.
    eventually("the panes are stacked", || {
        let (left, right) = (size_of("left"), size_of("right"));
        left.1 == 50 && right.1 == 50 && left.0 + right.0 + 4 == 23
    });

    // At 200 columns each pane is 86 wide, so they go side by side.
    tui.resize(30, 200);
    eventually("the panes are side by side", || {
        size_of("left") == (27, 84) && size_of("right") == (27, 84)
    });
}

#[test]
fn restart_server_brings_the_running_sessions_back() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "keeper", "sleep", "300"]);
    eventually("the session is saved", || {
        crystal.saved().contains("keeper")
    });
    let old_pid = crystal.pid("keeper");

    assert_eq!(crystal.ok(&["restart-server"]), "restarted the daemon\n");
    assert_eq!(crystal.row("keeper").unwrap()[1], "running");
    assert_ne!(crystal.pid("keeper"), old_pid);
    eventually("the old program has gone with its daemon", || {
        !alive(old_pid)
    });
}

#[test]
fn restart_server_without_a_daemon_starts_nothing() {
    let crystal = Crystal::new();
    assert_eq!(crystal.ok(&["restart-server"]), "no daemon was running\n");
    assert!(!crystal.socket.exists());
}

impl Crystal {
    /// Starts a daemon that takes itself for crystal 0.0.1, the way one left
    /// running from an older install would be.
    fn start_older_daemon(&self) -> std::process::Child {
        let daemon = self
            .command(&["daemon"])
            .env("CRYSTAL_PRETEND_VERSION", "0.0.1")
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        eventually("the daemon is listening", || self.socket.exists());
        daemon
    }
}

#[test]
fn a_daemon_of_another_version_says_how_to_restart_it() {
    let crystal = Crystal::new();
    let mut older = crystal.start_older_daemon();

    let err = crystal.fails(&["ls"]);
    let ours = env!("CARGO_PKG_VERSION");
    assert_eq!(
        err,
        format!(
            "crystal: this is crystal {ours}, but the daemon is crystal 0.0.1: \
             run `crystal restart-server` to restart the daemon on this crystal\n"
        )
    );

    // Restarting goes through whatever the version, and ends the mismatch.
    assert_eq!(crystal.ok(&["restart-server"]), "restarted the daemon\n");
    older.wait().unwrap();
    assert_eq!(crystal.ok(&["ls"]), "");
}

#[test]
fn kill_server_stops_a_daemon_of_another_version() {
    let crystal = Crystal::new();
    let mut older = crystal.start_older_daemon();
    crystal.ok(&["kill-server"]);
    older.wait().unwrap();
    assert!(!crystal.socket.exists());
}

#[test]
fn a_hook_stays_quiet_with_a_daemon_of_another_version() {
    let crystal = Crystal::new();
    let mut older = crystal.start_older_daemon();
    let mut child = crystal
        .command(&["hook", "claude"])
        .env("CRYSTAL_SESSION", "agent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
    assert!(out.stderr.is_empty());

    crystal.ok(&["kill-server"]);
    older.wait().unwrap();
}

/// A program that prints a marked first line and then enough lines to
/// scroll it well off the screen, then says it's done and waits.
const LONG_OUTPUT: &str =
    "echo first-line; for i in $(seq 1 60); do echo line $i; done; echo > printed; sleep 30";

#[test]
fn read_with_history_shows_what_scrolled_off_the_screen() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "printer", "sh", "-c", LONG_OUTPUT]);
    written(&crystal.dir.path().join("printed"));

    let screen = crystal.ok(&["read", "printer"]);
    assert!(!screen.lines().any(|line| line == "first-line"));
    assert!(screen.lines().any(|line| line == "line 60"));

    let all = crystal.ok(&["read", "printer", "--history"]);
    let lines: Vec<&str> = all.lines().collect();
    assert_eq!(lines[0], "first-line");
    assert_eq!(lines[60], "line 60");

    let last = crystal.ok(&["read", "printer", "--history", "--lines", "2"]);
    assert_eq!(last, "line 59\nline 60\n");
}

#[test]
fn rows_an_inline_agent_scrolls_up_through_a_region_reach_the_history() {
    let crystal = Crystal::new();
    // How an agent like Codex prints above its prompt: a scroll region from
    // the top of the screen down to just above the prompt, and each line
    // scrolled up through it.
    let script = r#"
        printf '\033[12;1H> the prompt\033[1;10r\033[10;1H'
        for i in $(seq 1 30); do printf '\r\nout %s' "$i"; done
        printf '\033[r'
        echo > printed
        sleep 30
    "#;
    crystal.ok(&["new", "-n", "inline", "sh", "-c", script]);
    written(&crystal.dir.path().join("printed"));

    let all = crystal.ok(&["read", "inline", "--history"]);
    let lines: Vec<&str> = all.lines().map(str::trim_end).collect();
    let first = lines.iter().position(|line| *line == "out 1");
    let last = lines.iter().position(|line| *line == "out 30");
    assert!(
        first.is_some() && first < last,
        "every line, in order:\n{all}"
    );
    assert!(
        lines.contains(&"> the prompt"),
        "the prompt stayed put:\n{all}"
    );
}

#[test]
fn the_tui_pages_back_through_output_from_before_it_opened_and_returns_to_live() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "printer", "sh", "-c", LONG_OUTPUT]);
    written(&crystal.dir.path().join("printed"));

    let mut tui = crystal.tui();
    tui.shows("line 40");
    assert!(!tui.text().contains("first-line"));

    // Page Up in the sidebar pages the selected session's pane, as far
    // back as the history goes.
    for _ in 0..5 {
        tui.type_keys("\x1b[5~");
    }
    tui.shows("first-line");
    tui.shows("↑");

    for _ in 0..5 {
        tui.type_keys("\x1b[6~");
    }
    eventually("the pane is live again", || {
        let text = tui.text();
        !text.contains("first-line") && !text.contains("↑")
    });
}

#[test]
fn typing_into_a_pane_brings_it_back_from_its_history() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "printer", "sh", "-c", LONG_OUTPUT]);
    written(&crystal.dir.path().join("printed"));

    let mut tui = crystal.tui();
    tui.shows("line 40");
    tui.type_keys("\r");
    tui.shows("typing into the session");

    // Shift+Page Up, as a terminal sends it.
    tui.type_keys("\x1b[5;2~");
    tui.shows("↑");
    tui.type_keys("x");
    eventually("the pane is live again", || !tui.text().contains("↑"));
}

#[test]
fn config_shows_the_settings_in_effect() {
    let crystal = Crystal::new();
    let out = crystal.ok(&["config"]);
    let path = crystal.config_file();
    assert!(out.starts_with(&format!("# {}\n", path.display())), "{out}");
    assert!(out.contains("notify = false"), "{out}");
    assert!(out.contains("new_session = \"claude\""), "{out}");

    std::fs::remove_file(&path).unwrap();
    let out = crystal.ok(&["config"]);
    assert!(out.contains("no file yet"), "{out}");
    assert!(out.contains("notify = true"), "{out}");
}

#[test]
fn a_setting_spelled_wrong_is_an_error_that_names_it_and_its_file() {
    let crystal = Crystal::new();
    crystal.configure("notfy = false\n");
    let err = crystal.fails(&["config"]);
    assert!(
        err.contains("notfy") && err.contains("config.toml"),
        "{err}"
    );

    let mut tui = crystal.terminal(&[]);
    tui.shows("notfy");
    assert!(!tui.exit());
}

#[test]
fn the_config_chooses_what_the_new_session_line_starts_with() {
    let crystal = Crystal::new();
    crystal.configure("notify = false\nnew_session = \"codex --full-auto\"\n");
    let mut tui = crystal.tui();
    tui.type_keys("n");
    tui.shows("new session: codex --full-auto");
}

/// A pretend agent that asks the user something once the test creates
/// `ask`, and finishes its turn once it creates `rest`, drawing what agents
/// draw for each.
const ASKING_AGENT: &str = r#"
    wait_for() { while [ ! -e "$1" ]; do sleep 0.05; done; }
    printf '> '
    wait_for ask; printf '\r\033[2KDo you want to proceed?'
    wait_for rest; printf '\r\033[2K> '
    sleep 30
"#;

impl Crystal {
    /// Has notices go to a file, one line each, rather than the desktop,
    /// and returns the file.
    fn notices_to_file(&self) -> PathBuf {
        let notices = self.dir.path().join("notices");
        let line = "$CRYSTAL_NOTICE_SESSION $CRYSTAL_NOTICE_ACTIVITY: $CRYSTAL_NOTICE";
        self.configure(&format!(
            "notify_command = '''echo \"{line}\" >> {}'''\n",
            notices.display()
        ));
        notices
    }
}

fn lines_in(file: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    text.lines().map(String::from).collect()
}

#[test]
fn the_user_is_told_once_each_time_a_session_comes_to_need_them() {
    let crystal = Crystal::new();
    let notices = crystal.notices_to_file();
    crystal.ok(&["new", "-n", "agent", "sh", "-c", ASKING_AGENT]);

    std::fs::write(crystal.dir.path().join("ask"), "").unwrap();
    eventually("the user is told it's waiting", || {
        lines_in(&notices) == ["agent waiting: agent is waiting on you"]
    });
    // It's still waiting a while later, which isn't news.
    thread::sleep(Duration::from_millis(800));
    assert_eq!(lines_in(&notices).len(), 1);

    std::fs::write(crystal.dir.path().join("rest"), "").unwrap();
    eventually("the user is told it's done", || {
        lines_in(&notices).last().map(String::as_str) == Some("agent done: agent is done")
    });
    assert_eq!(lines_in(&notices).len(), 2);
}

#[test]
fn nobody_is_told_about_a_session_someone_is_watching() {
    let crystal = Crystal::new();
    let notices = crystal.notices_to_file();
    crystal.ok(&["new", "-n", "agent", "sh", "-c", ASKING_AGENT]);

    let mut terminal = crystal.attach(&["attach", "agent"]);
    std::fs::write(crystal.dir.path().join("ask"), "").unwrap();
    eventually("the session is waiting", || {
        crystal.row("agent").unwrap()[1] == "waiting"
    });
    thread::sleep(Duration::from_millis(800));
    assert!(lines_in(&notices).is_empty());

    // Seen while it waited, it isn't news once the user looks away.
    terminal.type_keys("\x1c");
    assert!(terminal.exit());
    thread::sleep(Duration::from_millis(800));
    assert!(lines_in(&notices).is_empty());
}

/// What a terminal sends for a click at `(column, row)` on its screen,
/// counted from 0, the SGR way: a press, then a release.
fn click(column: usize, row: usize) -> String {
    let (x, y) = (column + 1, row + 1);
    format!("\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m")
}

/// What a terminal sends for a notch of the wheel, up, at `(column, row)`.
fn wheel_up(column: usize, row: usize) -> String {
    format!("\x1b[<64;{};{}M", column + 1, row + 1)
}

/// In the harness's 80-column terminal, the pane starts after the
/// 28-column sidebar, and its session's screen inside the pane's border.
const PANE_SCREEN_COLUMN: usize = 29;
const PANE_SCREEN_ROW: usize = 1;

#[test]
fn the_tui_hands_the_mouse_back_to_the_terminal_when_it_quits() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "stays", "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("▶ stays");
    assert!(tui.sends_the_mouse());
    tui.type_keys("q");
    assert!(tui.exit());
    assert!(!tui.sends_the_mouse());
}

#[test]
fn clicking_a_session_row_selects_it() {
    let crystal = Crystal::new();
    for name in ["alpha", "beta"] {
        let script = format!("echo {name} is here; echo > {name}-ready; sleep 30");
        crystal.ok(&["new", "-n", name, "sh", "-c", &script]);
        written(&crystal.dir.path().join(format!("{name}-ready")));
    }

    let mut tui = crystal.tui();
    tui.shows("alpha is here");
    let row = line_with(&tui.text(), "▶ beta");
    tui.type_keys(&click(10, row));
    tui.shows("beta is here");
}

#[test]
fn clicking_a_pane_hands_it_the_keyboard() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "cat", "cat"]);

    let mut tui = crystal.tui();
    tui.shows("▶ cat");
    tui.type_keys(&click(50, 10));
    tui.shows("typing into the session");
    tui.type_keys("hello by mouse\r");
    tui.shows("hello by mouse");
}

#[test]
fn the_wheel_over_a_pane_scrolls_it_back_through_its_history() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "printer", "sh", "-c", LONG_OUTPUT]);
    written(&crystal.dir.path().join("printed"));

    let mut tui = crystal.tui();
    tui.shows("line 40");
    tui.type_keys(&wheel_up(50, 10));
    tui.shows("↑ 3 lines");
}

#[test]
fn a_program_that_asks_for_the_mouse_gets_clicks_where_it_drew() {
    let crystal = Crystal::new();
    // Turns on mouse reporting the SGR way, then writes down the first
    // click it hears: a press and a release, 9 bytes each.
    let script = r"stty raw -echo; printf '\033[?1000h\033[?1006h'; echo > listening;
        dd bs=1 count=18 2>/dev/null > clicks; echo >> clicks; sleep 30";
    crystal.ok(&["new", "-n", "mousy", "sh", "-c", script]);
    written(&crystal.dir.path().join("listening"));

    let mut tui = crystal.tui();
    tui.shows("▶ mousy");
    // A program has the mouse in the pane that has the keyboard.
    tui.type_keys("\r");
    tui.shows("typing into the session");
    // Row 1, column 2 of the program's own screen.
    tui.type_keys(&click(PANE_SCREEN_COLUMN + 2, PANE_SCREEN_ROW + 1));

    let clicks = written(&crystal.dir.path().join("clicks"));
    assert_eq!(clicks, "\x1b[<0;3;2M\x1b[<0;3;2m\n");
}

#[test]
fn rename_gives_a_session_another_name() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);
    crystal.ok(&["new", "-n", "other", "sleep", "30"]);

    crystal.ok(&["rename", "agent", "reviewer"]);
    assert!(crystal.row("agent").is_none());
    assert_eq!(crystal.row("reviewer").unwrap()[1], "running");

    let taken = crystal.fails(&["rename", "reviewer", "other"]);
    assert!(taken.contains("already exists"), "{taken}");
    let spaced = crystal.fails(&["rename", "reviewer", "two words"]);
    assert!(spaced.contains("spaces"), "{spaced}");
    let missing = crystal.fails(&["rename", "nobody", "x"]);
    assert!(missing.contains("no session named nobody"), "{missing}");
}

#[test]
fn a_renamed_session_still_hears_from_its_agent() {
    let crystal = Crystal::new();
    let script = "echo $CRYSTAL_SESSION_ID > id; sleep 30";
    crystal.ok(&["new", "-n", "agent", "sh", "-c", script]);
    let id = written(&crystal.dir.path().join("id"));
    crystal.ok(&["rename", "agent", "reviewer"]);

    // The program's environment can't change, so it still says "agent";
    // its id finds the session whatever it's called now.
    let hook = format!("'{CRYSTAL}' hook claude");
    let session_env = [
        ("CRYSTAL_SESSION", "agent"),
        ("CRYSTAL_SESSION_ID", id.trim()),
    ];
    let event = r#"{"hook_event_name":"UserPromptSubmit"}"#;
    run_hook_with(&crystal, &session_env, &hook, event);
    assert_eq!(crystal.row("reviewer").unwrap()[1], "working");
}

#[test]
fn a_renamed_session_comes_back_under_its_new_name_after_a_restart() {
    let crystal = Crystal::new();
    let daemon = crystal.start_daemon();
    crystal.ok(&["new", "-n", "keeper", "sleep", "300"]);
    crystal.ok(&["rename", "keeper", "kept"]);
    eventually("the new name is saved", || {
        let saved = crystal.saved();
        saved.contains("\"kept\"") && !saved.contains("keeper")
    });

    crash(daemon);
    crystal.ok(&["new", "-n", "fresh", "sleep", "300"]);
    assert_eq!(crystal.row("kept").unwrap()[1], "running");
}

#[test]
fn r_in_the_tui_renames_the_selected_session() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("▶ agent");
    tui.type_keys("r");
    tui.shows("new name: agent");
    // Ctrl+U clears the old name first.
    tui.type_keys("\x15reviewer\r");
    tui.shows("▶ reviewer");
    assert!(crystal.row("reviewer").is_some());
}

#[test]
fn a_session_still_cannot_attach_to_itself_once_renamed() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "loop", "sh"]);

    let mut terminal = crystal.attach(&["attach", "loop"]);
    crystal.ok(&["rename", "loop", "looped"]);
    terminal.type_keys(&format!("{CRYSTAL} attach looped\r"));
    terminal.shows("can't attach looped to itself");
}

#[test]
fn the_tui_still_knows_the_session_it_runs_in_once_renamed() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "host", "sh"]);

    // The TUI, run inside the session called host, which it never shows.
    let mut terminal = crystal.attach(&["attach", "host"]);
    terminal.type_keys(&format!("{CRYSTAL}\r"));
    terminal.shows("This is the session crystal is running in.");

    crystal.ok(&["rename", "host", "renamed"]);
    terminal.shows("▶ renamed");
    terminal.shows("This is the session crystal is running in.");
}

#[test]
fn respawn_runs_an_ended_session_again_in_its_place() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "first", "sleep", "30"]);
    let script = "echo ran >> runs; exit 3";
    crystal.ok(&["new", "-n", "once", "sh", "-c", script]);
    crystal.ok(&["new", "-n", "last", "sleep", "30"]);
    eventually("once has ended", || {
        crystal.row("once").unwrap()[1] == "exited 3"
    });
    let running = crystal.fails(&["respawn", "first"]);
    assert!(running.contains("first is still running"), "{running}");

    crystal.ok(&["respawn", "once"]);
    let runs = crystal.dir.path().join("runs");
    eventually("once has run twice", || {
        std::fs::read_to_string(&runs).is_ok_and(|runs| runs == "ran\nran\n")
    });
    let listed = crystal.ok(&["ls"]);
    let names: Vec<&str> = listed
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(names, ["first", "once", "last"]);
}

#[test]
fn respawned_claude_picks_its_conversation_up_again() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = crystal
        .command(&["new", "-n", "agent", "claude"])
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

    // The agent's program ends on its own.
    let pid = crystal.pid("agent");
    // SAFETY: kill only sends a signal, to the group the agent leads.
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    eventually("the agent has ended", || {
        crystal.row("agent").unwrap()[1].starts_with("killed")
    });

    std::fs::remove_file(crystal.dir.path().join("args")).unwrap();
    let out = crystal
        .command(&["respawn", "agent"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let args = written(&crystal.dir.path().join("args"));
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(args[2..], ["--resume", "abc-123"]);
}

#[test]
fn enter_on_an_ended_session_starts_it_again_once_you_say_yes() {
    let crystal = Crystal::new();
    let script = "echo ran >> runs; exit 3";
    crystal.ok(&["new", "-n", "once", "sh", "-c", script]);
    eventually("once has ended", || {
        crystal.row("once").unwrap()[1] == "exited 3"
    });

    let mut tui = crystal.tui();
    tui.shows("■ once exited 3");
    tui.type_keys("\r");
    tui.shows("start once again? y/n");
    tui.type_keys("y");
    let runs = crystal.dir.path().join("runs");
    eventually("once has run twice", || {
        std::fs::read_to_string(&runs).is_ok_and(|runs| runs == "ran\nran\n")
    });
}

#[test]
fn worktree_rm_takes_the_sessions_that_ended_there_with_it() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&[
        "new", "-n", "fixer", "-c", repo_arg, "-w", "fix", "sh", "-c", "exit 0",
    ]);
    eventually("fixer has ended", || {
        crystal.row("fixer").unwrap()[1] == "exited 0"
    });

    crystal.ok(&["worktree", "rm", "app.worktrees/fix"]);
    assert!(!crystal.dir.path().join("app.worktrees/fix").exists());
    assert!(crystal.row("fixer").is_none());
}

#[test]
fn shift_w_removes_a_worktree_once_nothing_runs_in_it() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&[
        "new", "-n", "fixer", "-c", repo_arg, "-w", "fix", "sh", "-c", "exit 0",
    ]);
    let worktree = crystal.dir.path().join("app.worktrees/fix");
    let worktree_arg = worktree.to_str().unwrap();
    crystal.ok(&["new", "-n", "tests", "-c", worktree_arg, "sleep", "30"]);
    eventually("fixer has ended", || {
        crystal.row("fixer").unwrap()[1] == "exited 0"
    });

    // fixer, the first session in the worktree, is selected.
    let mut tui = crystal.tui();
    tui.shows("■ fixer");
    tui.type_keys("W");
    tui.shows("tests still running in fix");
    assert!(worktree.is_dir());

    crystal.ok(&["kill", "tests"]);
    tui.hides("▶ tests");
    tui.type_keys("W");
    tui.shows("remove worktree fix? y/n");
    tui.type_keys("y");
    eventually("the worktree is gone", || !worktree.exists());
    eventually("its ended session went with it", || {
        crystal.row("fixer").is_none()
    });
}
