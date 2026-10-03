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
        // the test sees, and no test pops up a real notification. Memory is
        // off unless a test turns it on, so Claude's arguments stay as each
        // test expects them.
        crystal.configure("notify = false\n\n[plugins]\nmemory = false\n");
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
        let written = Arc::new(Mutex::new(Vec::new()));
        let mut output = pty.master.try_clone_reader().unwrap();
        thread::spawn({
            let screen = screen.clone();
            let written = written.clone();
            move || {
                let mut buf = [0; 4096];
                while let Ok(n @ 1..) = output.read(&mut buf) {
                    screen.lock().unwrap().process(&buf[..n]);
                    written.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            }
        });
        Terminal {
            screen,
            written,
            keys: pty.master.take_writer().unwrap(),
            pty: pty.master,
            child,
        }
    }
}

struct Terminal {
    screen: Arc<Mutex<vt100::Parser>>,
    /// Everything crystal wrote to the terminal, for what vt100 doesn't
    /// keep, like a request to put text on the clipboard.
    written: Arc<Mutex<Vec<u8>>>,
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

    /// Whether crystal has asked this terminal to mark pastes as pastes.
    fn marks_pastes(&self) -> bool {
        self.screen.lock().unwrap().screen().bracketed_paste()
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

    /// Waits until crystal has asked the terminal, with OSC 52, to put
    /// `text` on the clipboard.
    fn copies(&self, text: &str) {
        let request = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let written = self.written.lock().unwrap().clone();
            if written
                .windows(request.len())
                .any(|window| window == request.as_bytes())
            {
                return;
            }
            let asked: Vec<String> = String::from_utf8_lossy(&written)
                .split("\x1b]52;c;")
                .skip(1)
                .map(|rest| rest.split('\x07').next().unwrap_or("").to_string())
                .collect();
            assert!(
                Instant::now() < deadline,
                "{text:?} was never copied; crystal asked for {asked:?}"
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

/// `bytes` in base64, as OSC 52 carries them.
fn base64(bytes: &[u8]) -> String {
    const LETTERS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(LETTERS[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
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
    assert_eq!(row[7], "sleep 30");
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
    assert_eq!(crystal.row("fails").unwrap()[7], "sh -c 'exit 3'");
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
fn attach_asks_your_terminal_for_what_the_program_asked_and_gives_it_back() {
    let crystal = Crystal::new();
    let script = r"printf '\033[?2004h\033[?1000h\033[?1006hasking'; sleep 30";
    crystal.ok(&["new", "-n", "asker", "sh", "-c", script]);

    let mut terminal = crystal.attach(&["attach", "asker"]);
    terminal.shows("asking");
    eventually("pastes are marked", || terminal.marks_pastes());
    assert!(terminal.sends_the_mouse());

    terminal.type_keys("\x1c");
    terminal.shows("[detached from asker]");
    assert!(!terminal.marks_pastes());
    assert!(!terminal.sends_the_mouse());
}

#[test]
fn attach_detaches_on_ctrl_backslash_in_the_kitty_keyboard_protocol() {
    let crystal = Crystal::new();
    let script = r"printf '\033[>1ukitty'; sleep 30";
    crystal.ok(&["new", "-n", "kitty", "sh", "-c", script]);

    let mut terminal = crystal.attach(&["attach", "kitty"]);
    terminal.shows("kitty");
    // How a terminal that speaks the protocol writes Ctrl+\.
    terminal.type_keys("\x1b[92;5u");
    terminal.shows("[detached from kitty]");
    assert!(terminal.exit());
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
    tui.shows("❯ alpha");
    tui.shows("❯ beta");
    tui.shows("alpha is here");

    tui.type_keys("j");
    tui.shows("beta is here");
}

#[test]
fn keys_go_to_the_pane_after_enter_and_back_to_the_list_after_ctrl_backslash() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "cat", "cat"]);

    let mut tui = crystal.tui();
    tui.shows("❯ cat");
    tui.type_keys("\r");
    tui.shows("typing into");
    tui.type_keys("hello pane\r");
    tui.shows("hello pane");

    tui.type_keys("\x1c");
    tui.shows("q quit");
    // Back on the list, q quits rather than going to cat.
    tui.type_keys("q");
    assert!(tui.exit());
}

#[test]
fn a_paste_goes_whole_to_the_pane_typed_into_and_never_to_the_list() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "cat", "sh", "-c", "cat > pasted"]);
    let pasted = crystal.dir.path().join("pasted");

    let mut tui = crystal.tui();
    tui.shows("❯ cat");
    // On the list, its letters would be keys: x would ask to kill cat.
    tui.type_keys("\x1b[200~x\x1b[201~");
    tui.type_keys("\r");
    tui.shows("typing into");
    tui.type_keys("\x1b[200~first line\rsecond line\r\x1b[201~");
    eventually("cat has both lines", || {
        std::fs::read_to_string(&pasted).is_ok_and(|text| text == "first line\nsecond line\n")
    });
    assert!(!tui.text().contains("kill cat?"));
}

#[test]
fn n_with_no_agent_installed_starts_a_shell_and_hands_it_the_keyboard() {
    let crystal = Crystal::new();
    let path = path_of(&[]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    assert!(crystal.socket.exists(), "the TUI starts the daemon");
    tui.shows("No sessions yet");

    tui.type_keys("n");
    tui.shows("New session · this directory");
    tui.shows("a shell takes no task");
    tui.shows("runs  your shell");
    tui.type_keys("\r");
    tui.shows("❯ sh");
    tui.shows("typing into");
    tui.type_keys("echo I am $CRYSTAL_SESSION\r");
    tui.shows("I am sh");
}

#[test]
fn n_starts_claude_with_its_hooks_and_the_task_as_its_prompt() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);

    tui.type_keys("n");
    tui.shows("What should it do?");
    tui.shows("Claude Code");
    tui.type_keys("fix the login bug");
    tui.shows("runs  claude -- 'fix the login bug'");
    tui.type_keys("\r");
    tui.shows("▸ claude");

    // Given a task, Claude is told how to close it, ahead of the prompt,
    // after a word on where that comes from. One argument a line, and a
    // blank line between the paragraphs.
    let args = written(&crystal.dir.path().join("args"));
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(args.len(), 8, "{args:?}");
    assert_eq!(args[0], "--settings");
    assert_eq!(args[2], "--append-system-prompt");
    assert!(
        args[3].starts_with("You're running inside crystal"),
        "{args:?}"
    );
    assert!(args[5].contains("crystal done"), "{args:?}");
    assert_eq!(args[6..], ["--", "fix the login bug"]);
}

#[test]
fn a_task_pasted_whole_keeps_its_lines() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    assert!(tui.marks_pastes());

    tui.type_keys("n");
    tui.shows("What should it do?");
    // A paste, the way a terminal sends one: its Enter doesn't start it.
    tui.type_keys("\x1b[200~fix the refund\rthen run the tests\x1b[201~");
    tui.shows("then run the tests");
    tui.type_keys("\r");
    tui.shows("▸ claude");

    let args = written(&crystal.dir.path().join("args"));
    assert!(
        args.ends_with("\nfix the refund\nthen run the tests\n"),
        "{args:?}"
    );
}

#[test]
fn codex_starts_with_the_model_chosen_and_the_task() {
    let crystal = Crystal::new();
    let bin = fake_codex(crystal.dir.path());
    let path = path_of(&[&bin]);
    let codex_home = crystal.dir.path().join("codex-home");
    let env = [
        ("PATH", path.as_str()),
        ("CODEX_HOME", codex_home.to_str().unwrap()),
        ("FAKE_CODEX_ID", "thread-1"),
    ];
    let mut tui = crystal.attach_with_env(&[], &env);

    tui.type_keys("n");
    tui.shows("Codex");
    // The models Codex lists, less the one it hides.
    tui.shows("gpt-test-mini");
    assert!(!tui.text().contains("gpt-internal"));
    tui.type_keys("add a test");
    // Tab to what runs, Tab to its model, and right to the first model.
    tui.type_keys("\t\t\x1b[C");
    tui.shows("runs  codex -m gpt-test-mini -- 'add a");
    tui.type_keys("\r");
    tui.shows("▸ codex");
    // Codex hears how to close its task at the end of its first prompt.
    let args = codex_args(&crystal);
    assert_eq!(args[..4], ["-m", "gpt-test-mini", "--", "add a test"]);
    assert!(args.last().unwrap().contains("crystal done"), "{args:?}");
    assert!(
        args.iter()
            .any(|line| line.starts_with("You're running inside crystal")),
        "{args:?}"
    );
}

#[test]
fn esc_closes_the_new_session_panel_and_starts_nothing() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);

    tui.type_keys("n");
    tui.shows("New session");
    tui.type_keys("never mind");
    tui.shows("never mind");
    tui.type_keys("\x1b");
    tui.hides("New session");
    tui.shows("No sessions yet");
    // Back on the list: q quits, so nothing was left waiting on the panel.
    tui.type_keys("q");
    assert!(tui.exit());
    assert!(!crystal.dir.path().join("args").exists());
    assert!(crystal.row("claude").is_none());
}

#[test]
fn ctrl_e_turns_the_panel_into_the_command_line_it_would_run() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);

    tui.type_keys("n");
    tui.shows("What should it do?");
    tui.type_keys("fix it\x05");
    tui.hides("New session");
    tui.shows("new session: claude -- 'fix it'");
    // Anything can be run from there.
    tui.type_keys("\x15sh -c 'echo ran > ran; sleep 30'\r");
    assert_eq!(written(&crystal.dir.path().join("ran")), "ran\n");
}

#[test]
fn a_profile_from_the_config_starts_with_its_options_and_prompt() {
    let crystal = Crystal::new();
    crystal.configure(
        r#"notify = false

[plugins]
memory = false

[[profile]]
name = "review"
description = "A second pair of eyes"
agent = "claude"
model = "opus"
mode = "plan"
args = ["--verbose"]
prompt = "Review the change."
"#,
    );
    let bin = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);

    tui.type_keys("n");
    tui.shows("review");
    // Profiles come first: Tab to what runs, then left from Claude Code.
    tui.type_keys("the refund fix\t\x1b[D");
    tui.shows("A second pair of eyes");
    tui.shows("runs  claude --model opus");
    tui.type_keys("\r");
    tui.shows("▸ claude");

    // After the hooks and what crystal tells Claude about its task.
    let args = written(&crystal.dir.path().join("args"));
    let lines: Vec<&str> = args.lines().collect();
    let options = lines.iter().position(|line| *line == "--model").unwrap();
    assert_eq!(
        lines[options..],
        [
            "--model",
            "opus",
            "--permission-mode",
            "plan",
            "--verbose",
            "--",
            "Review the change.",
            "",
            "the refund fix"
        ]
    );
}

#[test]
fn a_profile_s_instructions_are_added_to_claude_s_system_prompt() {
    let crystal = Crystal::new();
    crystal.configure(
        r#"notify = false

[[profile]]
name = "careful"
agent = "claude"
instructions = "Point out risks before anything else."
"#,
    );
    let bin = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);

    tui.type_keys("n");
    tui.shows("careful");
    tui.type_keys("tidy up\t\x1b[D");
    tui.shows("runs  claude --append-system-prompt");
    tui.type_keys("\r");
    tui.shows("▸ claude");

    // One argument a line: the instructions start the one after the option.
    let args = written(&crystal.dir.path().join("args"));
    let lines: Vec<&str> = args.lines().collect();
    let option = lines
        .iter()
        .position(|line| *line == "--append-system-prompt")
        .expect("claude was given a system prompt to add");
    assert_eq!(lines[option + 1], "Point out risks before anything else.");
    assert_eq!(lines.last(), Some(&"tidy up"));
}

#[test]
fn profiles_made_and_changed_in_the_tui_keep_the_config_s_comments() {
    let crystal = Crystal::new();
    crystal.configure(
        r#"# my own settings
notify = false

# the one for pull requests
[[profile]]
name = "review"
agent = "claude"
"#,
    );
    let bin = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    let file = crystal.config_file();
    let saved = |text: &str| {
        eventually(&format!("the config file has {text}"), || {
            std::fs::read_to_string(&file).is_ok_and(|toml| toml.contains(text))
        });
    };

    tui.type_keys("P");
    tui.shows("Profiles");
    tui.shows("review");
    // A new one: a, its name, Enter.
    tui.type_keys("a");
    tui.shows("New profile");
    tui.type_keys("quick\r");
    saved("name = \"quick\"");
    tui.shows("quick");
    // Up to review, change it: Tab to the description, then Enter.
    tui.type_keys("k\r");
    tui.shows("Profile · review");
    tui.type_keys("\tLooks it over\r");
    saved("description = \"Looks it over\"");
    tui.shows("Looks it over");

    let toml = std::fs::read_to_string(&file).unwrap();
    assert!(toml.starts_with("# my own settings\n"), "{toml}");
    assert!(
        toml.contains("# the one for pull requests\n[[profile]]\nname = \"review\""),
        "{toml}"
    );
}

#[test]
fn profile_lists_the_profiles_and_shows_what_one_runs() {
    let crystal = Crystal::new();
    crystal.configure(
        r#"notify = false

[[profile]]
name = "review"
description = "A second pair of eyes"
agent = "claude"
model = "opus"
instructions = "Be brief."
where = "worktree"

[[profile]]
name = "quick"
agent = "codex"
mode = "never"
instructions = "Keep changes small."
"#,
    );
    let listed = crystal.ok(&["profile"]);
    let rows: Vec<Vec<&str>> = listed
        .lines()
        .map(|line| {
            line.split("  ")
                .filter(|cell| !cell.is_empty())
                .map(str::trim)
                .collect()
        })
        .collect();
    assert_eq!(
        rows,
        [
            vec!["NAME", "AGENT", "WHERE", "DESCRIPTION"],
            vec!["review", "claude", "worktree", "A second pair of eyes"],
            vec!["quick", "codex", "-"],
        ]
    );
    // The README's example, word for word.
    assert_eq!(
        crystal.ok(&["profile", "show", "quick"]),
        "quick\n\
         agent   Codex\n\
         starts  wherever the new-session panel is set\n\
         runs    codex -a never -c 'developer_instructions=\"Keep changes small.\"' -- '<task>'\n"
    );

    let shown = crystal.ok(&["profile", "show", "review"]);
    assert!(shown.contains("agent   Claude Code\n"), "{shown}");
    assert!(shown.contains("starts  in a new worktree\n"), "{shown}");
    assert!(
        shown.contains(
            "runs    claude --model opus --append-system-prompt 'Be brief.' -- '<task>'\n"
        ),
        "{shown}"
    );
    let error = crystal.fails(&["profile", "show", "nope"]);
    assert!(error.contains("there's no profile called nope"), "{error}");
}

#[test]
fn a_leftover_preset_table_says_it_s_now_a_profile() {
    let crystal = Crystal::new();
    crystal.configure("[[preset]]\nname = \"review\"\nagent = \"claude\"\n");
    let error = crystal.fails(&["profile"]);
    assert!(
        error.contains("`[[preset]]` tables are now `[[profile]]`"),
        "{error}"
    );
}

#[test]
fn x_asks_first_and_only_y_kills_the_selected_session() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "doomed", "sleep", "30"]);
    let pid = crystal.pid("doomed");

    let mut tui = crystal.tui();
    tui.shows("❯ doomed");
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
fn a_closing_terminal_never_marks_a_session_seen() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "first", "sleep", "30"]);
    crystal.ok(&["new", "-n", "second", "sleep", "30"]);
    // A finished turn on the second session, waiting to be seen.
    for event in ["UserPromptSubmit", "Stop"] {
        let json = format!(r#"{{"hook_event_name":"{event}"}}"#);
        run_hook(&crystal, "second", &format!("{CRYSTAL} hook claude"), &json);
    }
    assert_eq!(crystal.row("second").unwrap()[1], "done");

    // A terminal that closes sends a line feed, Ctrl+J, on its way out. Had
    // it moved the selection, the second session would be shown, and so
    // seen, within moments; nothing to wait for, so give it half a second.
    let mut tui = crystal.tui();
    tui.shows("first");
    tui.type_keys("\n");
    thread::sleep(Duration::from_millis(500));
    assert_eq!(crystal.row("second").unwrap()[1], "done");
    tui.type_keys("q");
    assert!(tui.exit());
}

#[test]
fn q_quits_the_tui_and_the_sessions_keep_running() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "stays", "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("❯ stays");
    tui.type_keys("q");
    assert!(tui.exit());
    assert_eq!(crystal.row("stays").unwrap()[1], "running");
}

#[test]
fn a_question_mark_shows_every_key_and_the_next_key_only_closes_it() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "stays", "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("? keys");
    tui.type_keys("?");
    tui.shows("In the sidebar");
    tui.shows("next needing you");
    tui.shows("With the mouse");

    // q puts the keys away; it doesn't quit.
    tui.type_keys("q");
    eventually("the keys are put away", || {
        !tui.text().contains("In the sidebar")
    });
    tui.shows("❯ stays");
    tui.type_keys("q");
    assert!(tui.exit());
}

#[test]
fn an_ended_session_shows_how_it_ended() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "done", "sh", "-c", "echo last words; exit 3"]);
    eventually("the session has ended", || {
        crystal.row("done").unwrap()[1] == "exited 3"
    });

    let tui = crystal.tui();
    tui.shows("■ done · exited 3");
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

    // The terminal is 24 by 80. Beside the 28-column sidebar and its rule,
    // and between the top bar and the footer, below the pane's header line,
    // that leaves 21 by 51.
    let tui = crystal.tui();
    tui.shows("watching");
    eventually("the session is the pane's size", || size_is("21 51\n"));

    tui.resize(30, 100);
    eventually("the session follows the pane", || size_is("27 71\n"));
}

#[test]
fn z_zooms_the_pane_over_the_whole_screen_and_back() {
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

    let mut tui = crystal.tui();
    tui.shows("watching");
    eventually("the session is the pane's size", || size_is("21 51\n"));

    // Zoomed, the pane has all 80 columns: the sidebar has stepped aside.
    tui.type_keys("z");
    tui.shows("sizer · zoomed");
    eventually("the session is the zoomed pane's size", || {
        size_is("21 80\n")
    });
    tui.type_keys("z");
    tui.hides("zoomed");
    eventually("the session is the pane's size again", || {
        size_is("21 51\n")
    });
}

#[test]
fn f_floats_a_session_over_the_panes_and_puts_it_back() {
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

    let mut tui = crystal.tui();
    tui.shows("watching");
    eventually("the session is the pane's size", || size_is("21 51\n"));

    // Beside the sidebar there are 51 columns and 22 rows. The float's
    // frame takes all 51 and 17 of the rows; inside the frame, below its
    // header line, the session has 49 by 14.
    tui.type_keys("F");
    tui.shows("sizer · floating");
    tui.shows("typing into sizer");
    eventually("the session is the float's size", || size_is("14 49\n"));

    // Back in the sidebar, it goes on floating, until F puts it back.
    tui.type_keys("\x1c");
    tui.shows("F puts it back");
    tui.type_keys("F");
    tui.hides("floating");
    eventually("the session is the pane's size again", || {
        size_is("21 51\n")
    });
}

#[test]
fn keys_go_to_the_session_that_floats() {
    let crystal = Crystal::new();
    crystal.ok(&[
        "new",
        "-n",
        "reader",
        "sh",
        "-c",
        "read line; echo \"$line\" > got; sleep 30",
    ]);

    let mut tui = crystal.tui();
    tui.shows("❯ reader");
    tui.type_keys("F");
    tui.shows("typing into reader");
    tui.type_keys("hello float\r");
    assert_eq!(written(&crystal.dir.path().join("got")), "hello float\n");
}

/// Opens the TUI the way it runs over ssh, so that what it copies goes to
/// the terminal, with OSC 52, rather than to this machine's clipboard.
fn tui_over_ssh(crystal: &Crystal) -> Terminal {
    crystal.attach_with_env(&[], &[("SSH_TTY", "/dev/ttys999")])
}

#[test]
fn copy_mode_finds_text_in_the_history_and_copies_it() {
    let crystal = Crystal::new();
    let script = "for i in $(seq 1 60); do echo row $i; done; echo the needle is here; \
                  for i in $(seq 61 120); do echo row $i; done; sleep 30";
    crystal.ok(&["new", "-n", "printer", "sh", "-c", script]);

    let mut tui = tui_over_ssh(&crystal);
    tui.shows("row 120");
    tui.type_keys("v");
    tui.shows("copying from printer");
    tui.type_keys("?needle");
    tui.shows("search up: needle");
    tui.type_keys("\r");
    tui.shows("the needle is here");
    tui.shows("needle: 1 of 1");

    // The search left the cursor on "needle": select to the end of the
    // line, and copy it.
    tui.type_keys("v$y");
    tui.copies("needle is here");
    tui.shows("copied 1 line");
    // Copy mode is over, and the keyboard is back in the sidebar.
    tui.type_keys("j");
    tui.shows("q quit");
}

#[test]
fn e_opens_a_sessions_history_in_the_editor() {
    let crystal = Crystal::new();
    // 120 rows, most of them in the history by the end, and a line longer
    // than the pane is wide, which wraps onto two rows.
    let long = "word ".repeat(16);
    let printing = format!(
        "for i in $(seq 1 120); do echo row $i; done; echo '{long}'; echo the end; sleep 30"
    );
    crystal.ok(&["new", "-n", "printer", "sh", "-c", &printing]);
    // An editor that keeps a copy of the file it was asked to open.
    let editor = crystal.dir.path().join("editor");
    let edited = crystal.dir.path().join("edited");
    script(
        &editor,
        "cp \"$1\" \"$EDITED.new\" && mv \"$EDITED.new\" \"$EDITED\"\nsleep 30\n",
    );

    let mut tui = crystal.attach_with_env(
        &[],
        &[
            ("EDITOR", editor.to_str().unwrap()),
            ("EDITED", edited.to_str().unwrap()),
        ],
    );
    tui.shows("the end");
    tui.type_keys("e");
    tui.shows("typing into printer-history");
    let rows: Vec<String> = (1..=120).map(|i| format!("row {i}")).collect();
    let expected = format!("{}\n{}\nthe end\n", rows.join("\n"), long.trim_end());
    assert_eq!(written(&edited), expected);
}

#[test]
fn a_drag_across_a_pane_copies_what_it_covers() {
    let crystal = Crystal::new();
    crystal.ok(&[
        "new",
        "-n",
        "words",
        "sh",
        "-c",
        "echo alpha beta gamma; sleep 30",
    ]);

    let mut tui = tui_over_ssh(&crystal);
    tui.shows("alpha beta gamma");
    // The pane's screen starts at column 30, row 3, counting from 1 as the
    // mouse does: "beta" is at columns 36 to 39 of the first row. Down on
    // its first letter, drag to its last, and let go.
    tui.type_keys("\x1b[<0;36;3M\x1b[<32;39;3M\x1b[<0;39;3m");
    tui.copies("beta");
    tui.shows("copied 1 line");
}

#[test]
fn the_tui_needs_a_terminal() {
    let crystal = Crystal::new();
    assert!(crystal.fails(&[]).contains("crystal needs a terminal"));
    assert!(!crystal.socket.exists());
}

/// A PATH of `bins` and the system's own directories, and nothing else:
/// the new-session panel offers the agents it finds on the PATH, and a
/// test must find only its stand-ins, never a real agent.
fn path_of(bins: &[&Path]) -> String {
    let mut dirs: Vec<String> = bins.iter().map(|bin| bin.display().to_string()).collect();
    dirs.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(String::from));
    dirs.join(":")
}

/// A stand-in for Claude Code: a `claude` that writes down the arguments
/// it was started with, one per line, and waits. Returns the directory to
/// put on the PATH. The arguments go to a file of their own first and are
/// then moved into place, so a test never reads half of them.
fn fake_claude(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > args.new && mv args.new args\nsleep 30\n",
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
    let said = hook_says(crystal, session_env, hook, event);
    assert!(said.is_empty(), "the hook printed for {event}: {said}");
}

/// Runs a hook the way [`run_hook_with`] does, and gives back what it
/// printed, which Claude Code reads. A hook must succeed.
fn hook_says(crystal: &Crystal, session_env: &[(&str, &str)], hook: &str, event: &str) -> String {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(hook)
        // Not the id of a session these tests are run in.
        .env_remove("CRYSTAL_SESSION_ID")
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
    String::from_utf8(out.stdout).unwrap()
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
    assert_eq!(crystal.row("agent").unwrap()[7], "claude --resume");

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

impl Crystal {
    /// Starts a pretend agent: `body` as a shell script on the PATH under
    /// the name of an agent crystal knows, so that crystal takes it for an
    /// agent and reads its screen. A plain `sh -c` would be a shell, whose
    /// screen says nothing about what an agent is doing.
    fn new_pretend_agent(&self, name: &str, body: &str) {
        let bin = self.dir.path().join("pretend-bin");
        std::fs::create_dir_all(&bin).unwrap();
        script(&bin.join("aider"), body);
        let out = self
            .command(&["new", "-n", name, "aider"])
            .env("PATH", path_of(&[&bin]))
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{err}");
    }
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
    crystal.new_pretend_agent("agent", script);
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

/// The TUI's sidebar out of the whole screen's `text`: each line up to
/// the rule after the sidebar's 28 columns, leaving out the pane beside it,
/// whose header can name a session too.
fn sidebar_of(text: &str) -> String {
    let lines: Vec<String> = text
        .lines()
        .map(|line| line.chars().take(29).collect())
        .collect();
    lines.join("\n")
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
    tui.shows("❯ shell");
    let text = sidebar_of(&tui.text());
    let order = [
        line_with(&text, " app ─"),
        line_with(&text, "⌂ main"),
        line_with(&text, "❯ planner"),
        line_with(&text, "⎇ fix"),
        line_with(&text, "❯ fixer"),
        line_with(&text, "outside git"),
        line_with(&text, "❯ shell"),
    ];
    assert!(order.is_sorted(), "out of order: {order:?}\n{text}");
}

#[test]
fn w_names_the_new_worktrees_branch_after_the_task() {
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

    let bin = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("▸ planner");
    tui.type_keys("w");
    tui.shows("New session · app ⎇ a new branch");
    tui.type_keys("Fix the flaky test!");
    tui.shows("New session · app ⎇ fix-the-flaky-test");
    tui.shows("branch       fix-the-flaky-test");
    tui.type_keys("\r");
    tui.shows("⎇ fix-the-flaky-test");
    tui.shows("typing into");

    // Claude Code started in the new worktree, which writes down its
    // arguments where it runs.
    let worktree = crystal.dir.path().join("app.worktrees/fix-the-flaky-test");
    let args = written(&worktree.join("args"));
    assert_eq!(args.lines().last(), Some("Fix the flaky test!"));
}

#[test]
fn a_new_worktree_with_no_task_asks_what_to_call_its_branch() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);

    let path = path_of(&[]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("▸ planner");
    tui.type_keys("w\r");
    tui.shows("name the new worktree's branch");
    tui.type_keys("spike\r");
    // The panel's title shows the branch as it's typed, before Enter.
    let worktree = crystal.dir.path().join("app.worktrees/spike");
    eventually("the worktree is made", || worktree.is_dir());
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
fn send_keys_presses_keys_never_pasting_them() {
    let crystal = Crystal::new();
    // The same program that asks for bracketed paste: answering an agent's
    // question has to reach it as keys, which agents act on, not a paste.
    // It keeps the first 8 bytes: `1`, Escape, Up, `yes`.
    let script = r"stty raw -echo; printf '\033[?2004hready'; head -c 8 > received; echo >> received; sleep 30";
    crystal.ok(&["new", "-n", "asker", "sh", "-c", script]);
    shows_on_screen(&crystal, "asker", "ready");

    crystal.ok(&["send-keys", "asker", "1", "Escape", "Up", "yes"]);
    assert_eq!(
        written(&crystal.dir.path().join("received")),
        "1\x1b\x1b[Ayes\n"
    );
}

#[test]
fn send_keys_speaks_the_kitty_keyboard_protocol_to_a_program_that_asks() {
    let crystal = Crystal::new();
    // A program that pushes the protocol's first flag, which tells apart
    // keys the old way can't, like Escape. It keeps the first 14 bytes:
    // Escape, Ctrl+C, `ok`.
    let script = r"stty raw -echo; printf '\033[>1uready'; head -c 14 > received; echo >> received; sleep 30";
    crystal.ok(&["new", "-n", "kitty", "sh", "-c", script]);
    shows_on_screen(&crystal, "kitty", "ready");

    crystal.ok(&["send-keys", "kitty", "Escape", "C-c", "ok"]);
    assert_eq!(
        written(&crystal.dir.path().join("received")),
        "\x1b[27u\x1b[99;5uok\n"
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
    crystal.new_pretend_agent("agent", script);

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
    tui.shows("❯ reader");
    tui.type_keys("s");
    tui.type_keys("j");

    // The first Tab goes to the selection's pane, the next to the split.
    tui.type_keys("\t");
    tui.shows("typing into");
    tui.type_keys("\x1c");
    tui.shows("q quit");
    tui.type_keys("\t");
    tui.shows("typing into");
    tui.type_keys("hello split\r");

    assert_eq!(written(&crystal.dir.path().join("got")), "hello split\n");
}

/// The row of the screen `text` first shows on, counted from 0.
fn row_of(tui: &Terminal, text: &str) -> Option<usize> {
    tui.text().lines().position(|row| row.contains(text))
}

#[test]
fn l_moves_a_pane_past_the_next_and_the_tab_keeps_the_order() {
    let crystal = Crystal::new();
    for name in ["alpha", "beta"] {
        let script = format!("echo {name} is here; echo > {name}-ready; sleep 30");
        crystal.ok(&["new", "-n", name, "sh", "-c", &script]);
        written(&crystal.dir.path().join(format!("{name}-ready")));
    }

    // At 80 columns the panes are stacked: beta's, which follows the
    // selection, on top, then alpha's split.
    let mut tui = crystal.tui();
    tui.shows("alpha is here");
    tui.type_keys("sj");
    tui.shows("beta is here");
    let above = |tui: &Terminal, first: &str, second: &str| matches!((row_of(tui, first), row_of(tui, second)), (Some(a), Some(b)) if a < b);
    assert!(
        above(&tui, "beta is here", "alpha is here"),
        "{}",
        tui.text()
    );

    tui.type_keys("L");
    eventually("beta's pane goes below alpha's", || {
        above(&tui, "alpha is here", "beta is here")
    });

    // The order is the tab's: it's there again when the TUI opens.
    tui.type_keys("q");
    assert!(tui.exit());
    let tui = crystal.tui();
    tui.shows("beta is here");
    tui.shows("alpha is here");
    assert!(
        above(&tui, "alpha is here", "beta is here"),
        "{}",
        tui.text()
    );
}

#[test]
fn a_layout_saved_puts_the_tabs_back_the_way_they_were() {
    let crystal = Crystal::new();
    for name in ["alpha", "beta"] {
        let script = format!("echo {name} is here; echo > {name}-ready; sleep 30");
        crystal.ok(&["new", "-n", name, "sh", "-c", &script]);
        written(&crystal.dir.path().join(format!("{name}-ready")));
    }

    // alpha split off beside beta, in a tab called review.
    let mut tui = crystal.tui();
    tui.shows("alpha is here");
    tui.type_keys("sj");
    tui.shows("beta is here");
    tui.type_keys("Treview\r");
    tui.shows("1 review");

    tui.type_keys("S");
    tui.shows("no layouts yet");
    tui.type_keys("s");
    tui.shows("save the tabs as:");
    tui.type_keys("side by side\r");
    tui.shows("saved your tabs as side by side");
    tui.shows("1 tab · 2 sessions");
    tui.type_keys("\x1b");
    tui.hides("layouts ·");

    // The split closes, and the tab loses its name.
    tui.type_keys("k");
    tui.type_keys("s");
    tui.hides("beta is here");
    tui.type_keys("T\x15\r");
    tui.hides("review");

    tui.type_keys("S");
    tui.shows("side by side");
    tui.type_keys("\r");
    tui.shows("restored side by side");
    tui.shows("1 review");
    tui.shows("alpha is here");
    tui.shows("beta is here");

    // The tabs it replaced are kept, to go back to.
    tui.type_keys("S");
    tui.shows("↶ before side by side");
    assert!(crystal.dir.path().join("layouts.json").exists());
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

    // At 24 by 80 there are 51 columns beside the sidebar, too few to share
    // side by side, so the panes are stacked: each 51 columns wide, with
    // the 22 rows between the top bar and the footer shared between them,
    // less a header line each.
    eventually("the panes are stacked", || {
        let (left, right) = (size_of("left"), size_of("right"));
        left.1 == 51 && right.1 == 51 && left.0 + right.0 + 2 == 22
    });

    // At 200 columns there are 171 beside the sidebar: two panes of 85,
    // with a rule between them, so they go side by side.
    tui.resize(30, 200);
    eventually("the panes are side by side", || {
        size_of("left") == (27, 85) && size_of("right") == (27, 85)
    });
}

/// Starts a session for each of `names` that says it's here, and waits
/// until each has.
fn sessions_saying_here(crystal: &Crystal, names: &[&str]) {
    for name in names {
        let script = format!("echo {name} is here; echo > {name}-ready; sleep 30");
        crystal.ok(&["new", "-n", name, "sh", "-c", &script]);
        written(&crystal.dir.path().join(format!("{name}-ready")));
    }
}

/// Waits until the TUI's sidebar shows `text`.
fn sidebar_shows(tui: &Terminal, text: &str) {
    eventually(&format!("the sidebar shows {text:?}"), || {
        sidebar_of(&tui.text()).contains(text)
    });
}

/// Waits until the TUI's sidebar no longer shows `text`.
fn sidebar_hides(tui: &Terminal, text: &str) {
    eventually(&format!("the sidebar doesn't show {text:?}"), || {
        !sidebar_of(&tui.text()).contains(text)
    });
}

/// Presses `t` in the TUI: a new tab, with a shell in it, which has the
/// keyboard. Waits for the shell, then gives the sidebar the keyboard back.
fn new_tab_with_a_shell(tui: &mut Terminal) {
    tui.type_keys("t");
    // Keys typed before the pane shows the shell's prompt would be lost.
    tui.shows("$ ");
    tui.type_keys("echo in the new tab\r");
    tui.shows("in the new tab");
    sidebar_shows(tui, "❯ sh");
    tui.type_keys("\x1c");
}

#[test]
fn t_makes_a_tab_of_its_own_and_going_between_tabs_changes_the_sidebar() {
    let crystal = Crystal::new();
    sessions_saying_here(&crystal, &["alpha", "beta"]);

    let mut tui = crystal.tui();
    tui.shows("alpha is here");
    new_tab_with_a_shell(&mut tui);
    // The new tab has its shell and nothing else.
    sidebar_hides(&tui, "❯ alpha");
    sidebar_hides(&tui, "❯ beta");
    tui.shows(" 1  2 ");

    tui.type_keys("1");
    sidebar_shows(&tui, "❯ alpha");
    sidebar_shows(&tui, "❯ beta");
    sidebar_hides(&tui, "❯ sh");
    tui.shows("alpha is here");

    // The second tab's label, " 2 ", is drawn from column 13 of the top
    // bar.
    tui.type_keys(&click(14, 0));
    sidebar_shows(&tui, "❯ sh");
    sidebar_hides(&tui, "❯ alpha");
    tui.shows("in the new tab");
}

#[test]
fn a_session_moved_to_another_tab_leaves_this_one() {
    let crystal = Crystal::new();
    sessions_saying_here(&crystal, &["alpha", "beta"]);

    let mut tui = crystal.tui();
    tui.shows("alpha is here");
    new_tab_with_a_shell(&mut tui);
    tui.type_keys("1");
    sidebar_shows(&tui, "❯ alpha");

    tui.type_keys(">");
    tui.shows("move alpha to tab 1-9");
    tui.type_keys("2");
    tui.shows("moved alpha to tab 2");
    sidebar_hides(&tui, "❯ alpha");
    tui.shows("beta is here");

    tui.type_keys("2");
    sidebar_shows(&tui, "❯ alpha");
    sidebar_shows(&tui, "❯ sh");
}

#[test]
fn an_agent_waiting_in_another_tab_shows_on_its_tab_and_u_goes_there() {
    let crystal = Crystal::new();
    sessions_saying_here(&crystal, &["alpha"]);
    let mut tui = crystal.tui();
    tui.shows("alpha is here");
    new_tab_with_a_shell(&mut tui);

    // Started from the command line, the agent joins the tab in front.
    let bin = fake_claude(crystal.dir.path());
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = crystal
        .command(&["new", "-n", "agent", "claude"])
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(out.status.success());
    sidebar_shows(&tui, "agent");

    tui.type_keys("1");
    sidebar_hides(&tui, "agent");
    let args = written(&crystal.dir.path().join("args"));
    let settings: serde_json::Value = serde_json::from_str(args.lines().nth(1).unwrap()).unwrap();
    let hook = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    run_hook(
        &crystal,
        "agent",
        hook,
        r#"{"hook_event_name":"PermissionRequest"}"#,
    );
    tui.shows(" 2 ▲ ");

    tui.type_keys("u");
    sidebar_shows(&tui, "▲ agent");
    sidebar_hides(&tui, "❯ alpha");
}

#[test]
fn closing_a_tab_with_a_session_in_it_asks_then_kills_the_session() {
    let crystal = Crystal::new();
    sessions_saying_here(&crystal, &["alpha"]);

    let mut tui = crystal.tui();
    tui.shows("alpha is here");
    new_tab_with_a_shell(&mut tui);
    assert!(crystal.row("sh").is_some());

    tui.type_keys("&");
    tui.shows("close tab 2 and kill its 1 session? y/n");
    tui.type_keys("y");
    sidebar_shows(&tui, "❯ alpha");
    tui.hides(" 2 ");
    eventually("the tab's shell is killed", || crystal.row("sh").is_none());
    assert_eq!(crystal.row("alpha").unwrap()[1], "running");
}

#[test]
fn tabs_come_back_when_the_tui_opens_again() {
    let crystal = Crystal::new();
    sessions_saying_here(&crystal, &["alpha", "beta"]);

    let mut tui = crystal.tui();
    tui.shows("alpha is here");
    new_tab_with_a_shell(&mut tui);
    tui.type_keys("T");
    tui.shows("tab name:");
    tui.type_keys("review\r");
    tui.shows(" 2 review ");
    tui.type_keys("q");
    assert!(tui.exit());

    // Back in the named tab, with its shell alone; the first has the rest.
    let mut tui = crystal.tui();
    tui.shows(" 2 review ");
    sidebar_shows(&tui, "❯ sh");
    sidebar_hides(&tui, "❯ alpha");
    tui.type_keys("1");
    sidebar_shows(&tui, "❯ alpha");
    sidebar_shows(&tui, "❯ beta");
    sidebar_hides(&tui, "❯ sh");
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
fn a_claude_session_picked_up_again_isn_t_asked_its_task_again() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let out = crystal
        .command(&["new", "-n", "agent", "-t", "fix the login bug", "claude"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let args = written(&crystal.dir.path().join("args"));
    assert!(args.ends_with("--\nfix the login bug\n"), "{args}");

    // Claude's hooks name its conversation, whose transcript exists once
    // the task has been sent.
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

    std::fs::remove_file(crystal.dir.path().join("args")).unwrap();
    let out = crystal
        .command(&["restart-server"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());

    // Back in its conversation, which has had the task already.
    let args = written(&crystal.dir.path().join("args"));
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(args[2..4], ["--resume", "abc-123"]);
    assert!(!args.contains(&"fix the login bug"), "{args:?}");
    assert!(!args.contains(&"--"), "{args:?}");
    // Still a task, open as it was.
    assert_eq!(crystal.row("agent").unwrap()[8], "fix the login bug");
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
    tui.shows("line 60");
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
    tui.shows("line 60");
    tui.type_keys("\r");
    tui.shows("typing into");

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
fn the_config_chooses_what_the_new_session_panel_starts_with() {
    let crystal = Crystal::new();
    crystal.configure("notify = false\nnew_session = \"codex --full-auto\"\n");
    let claude = fake_claude(crystal.dir.path());
    let codex = fake_codex(crystal.dir.path());
    let path = path_of(&[&claude, &codex]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.type_keys("n");
    tui.shows("runs  codex --full-auto");
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
    crystal.new_pretend_agent("agent", ASKING_AGENT);

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
    crystal.new_pretend_agent("agent", ASKING_AGENT);

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
const PANE_SCREEN_ROW: usize = 2;

#[test]
fn the_tui_hands_the_mouse_back_to_the_terminal_when_it_quits() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "stays", "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("❯ stays");
    assert!(tui.sends_the_mouse());
    assert!(tui.marks_pastes());
    tui.type_keys("q");
    assert!(tui.exit());
    assert!(!tui.sends_the_mouse());
    assert!(!tui.marks_pastes(), "pastes go back to plain typing too");
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
    let row = line_with(&tui.text(), "❯ beta");
    tui.type_keys(&click(10, row));
    tui.shows("beta is here");
}

#[test]
fn clicking_a_pane_hands_it_the_keyboard() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "cat", "cat"]);

    let mut tui = crystal.tui();
    tui.shows("❯ cat");
    tui.type_keys(&click(50, 10));
    tui.shows("typing into");
    tui.type_keys("hello by mouse\r");
    tui.shows("hello by mouse");
}

#[test]
fn the_wheel_over_a_pane_scrolls_it_back_through_its_history() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "printer", "sh", "-c", LONG_OUTPUT]);
    written(&crystal.dir.path().join("printed"));

    let mut tui = crystal.tui();
    tui.shows("line 60");
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
    tui.shows("❯ mousy");
    // A program has the mouse in the pane that has the keyboard.
    tui.type_keys("\r");
    tui.shows("typing into");
    // Row 1, column 2 of the program's own screen.
    tui.type_keys(&click(PANE_SCREEN_COLUMN + 2, PANE_SCREEN_ROW + 1));

    let clicks = written(&crystal.dir.path().join("clicks"));
    assert_eq!(clicks, "\x1b[<0;3;2M\x1b[<0;3;2m\n");
}

#[test]
fn a_pane_speaks_the_kitty_keyboard_protocol_to_a_program_that_asks() {
    let crystal = Crystal::new();
    // Pushes the protocol's first flag, then writes down the first key it
    // hears: Escape, which the protocol writes as an escape of its own.
    let script = r"stty raw -echo; printf '\033[>1u'; echo > listening;
        dd bs=1 count=5 2>/dev/null > key; echo >> key; sleep 30";
    crystal.ok(&["new", "-n", "kitty", "sh", "-c", script]);
    written(&crystal.dir.path().join("listening"));

    let mut tui = crystal.tui();
    tui.shows("❯ kitty");
    tui.type_keys("\r");
    tui.shows("typing into");
    tui.type_keys("\x1b");
    assert_eq!(written(&crystal.dir.path().join("key")), "\x1b[27u\n");
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
    tui.shows("❯ agent");
    tui.type_keys("r");
    tui.shows("new name: agent");
    // Ctrl+U clears the old name first.
    tui.type_keys("\x15reviewer\r");
    tui.shows("❯ reviewer");
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
    terminal.shows("❯ renamed");
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
    tui.shows("■ once · exited 3");
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
    tui.hides("❯ tests");
    tui.type_keys("W");
    tui.shows("remove worktree fix? y/n");
    tui.type_keys("y");
    eventually("the worktree is gone", || !worktree.exists());
    eventually("its ended session went with it", || {
        crystal.row("fixer").is_none()
    });
}

#[test]
fn a_worktree_whose_last_session_is_killed_stays_until_shift_w_removes_it() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);
    crystal.ok(&[
        "new", "-n", "fixer", "-c", repo_arg, "-w", "fix", "sleep", "30",
    ]);
    let worktree = crystal.dir.path().join("app.worktrees/fix");

    let mut tui = crystal.tui();
    tui.shows("❯ fixer");
    // planner, in the main worktree, comes first.
    tui.type_keys("jx");
    tui.shows("kill fixer? y/n");
    tui.type_keys("y");
    tui.hides("❯ fixer");
    // The worktree stays, and the selection is on it.
    tui.shows("· no sessions");
    tui.shows("No sessions in ⎇ fix");
    assert!(worktree.is_dir());

    tui.type_keys("W");
    tui.shows("remove worktree fix? y/n");
    tui.type_keys("y");
    eventually("the worktree is gone", || !worktree.exists());
    tui.hides("no sessions");
    tui.hides("⎇ fix");
}

#[test]
fn shift_w_on_a_worktree_with_work_in_it_says_why_git_keeps_it() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);
    crystal.ok(&["new", "-n", "fixer", "-c", repo_arg, "-w", "fix", "true"]);
    let worktree = crystal.dir.path().join("app.worktrees/fix");
    std::fs::write(worktree.join("notes.txt"), "half done\n").unwrap();
    crystal.ok(&["kill", "fixer"]);

    let mut tui = crystal.tui();
    tui.shows("· no sessions");
    tui.type_keys("j");
    tui.shows("No sessions in ⎇ fix");
    tui.type_keys("W");
    tui.shows("remove worktree fix? y/n");
    tui.type_keys("y");
    tui.shows("'fix' contains modified or untracked files");
    assert!(worktree.join("notes.txt").exists());
    tui.shows("· no sessions");
}

#[test]
fn ls_json_lists_every_session_with_its_status() {
    let crystal = Crystal::new();
    assert_eq!(crystal.ok(&["ls", "--json"]).trim(), "[]");

    crystal.ok(&["new", "-n", "worker", "sleep", "30"]);
    crystal.ok(&["new", "-n", "quitter", "sh", "-c", "exit 3"]);
    eventually("quitter has ended", || {
        crystal.row("quitter").unwrap()[1] == "exited 3"
    });

    let listed: serde_json::Value = serde_json::from_str(&crystal.ok(&["ls", "--json"])).unwrap();
    let sessions = listed.as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    let worker = &sessions[0];
    assert_eq!(worker["name"], "worker");
    assert_eq!(worker["status"], "running");
    assert_eq!(worker["state"], "running");
    assert_eq!(worker["command"], serde_json::json!(["sleep", "30"]));
    assert!(worker["id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(worker["pid"].as_u64().is_some());
    assert!(worker["cwd"].as_str().is_some());
    assert_eq!(sessions[1]["status"], "exited 3");
}

#[test]
fn skill_install_writes_the_skill_and_keeps_a_changed_one() {
    let crystal = Crystal::new();
    let claude_dir = crystal.dir.path().join("claude-config");
    let install = |args: &[&str]| {
        crystal
            .command(args)
            .env("CLAUDE_CONFIG_DIR", &claude_dir)
            .output()
            .unwrap()
    };
    let skill_file = claude_dir.join("skills/crystal/SKILL.md");
    let printed = crystal.ok(&["skill"]);
    assert!(printed.starts_with("---\nname: crystal\n"));

    let out = install(&["skill", "--install"]);
    assert!(out.status.success());
    assert_eq!(std::fs::read_to_string(&skill_file).unwrap(), printed);
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains(skill_file.to_str().unwrap()), "{said}");

    // Changed by the user, it's kept unless they say otherwise.
    std::fs::write(&skill_file, "my own notes\n").unwrap();
    let refused = install(&["skill", "--install"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--force"));
    assert_eq!(
        std::fs::read_to_string(&skill_file).unwrap(),
        "my own notes\n"
    );

    assert!(install(&["skill", "--install", "--force"]).status.success());
    assert_eq!(std::fs::read_to_string(&skill_file).unwrap(), printed);
}

/// A crystal with memory on, and a project for it to remember things about.
fn crystal_remembering() -> (Crystal, PathBuf) {
    let crystal = Crystal::new();
    crystal.configure("notify = false\n");
    let repo = git_repo(crystal.dir.path(), "app");
    (crystal, repo)
}

#[test]
fn remember_inside_a_session_keeps_the_entry_for_the_session_s_project() {
    let (crystal, repo) = crystal_remembering();
    // The session is in a worktree of its own; what it remembers is the
    // whole project's.
    let remember = format!(
        "'{CRYSTAL}' remember -k gotcha 'The ledger tests need the database up' > said; sleep 30"
    );
    let repo_dir = repo.to_str().unwrap();
    crystal.ok(&[
        "new",
        "-n",
        "fixer",
        "-c",
        repo_dir,
        "-w",
        "fix/ledger",
        "sh",
        "-c",
        &remember,
    ]);
    let worktree = crystal.dir.path().join("app.worktrees/fix-ledger");
    assert_eq!(written(&worktree.join("said")), "remembered 1\n");

    let listed = crystal.ok(&["memory", "-C", repo_dir]);
    assert!(listed.contains("gotcha"), "{listed}");
    assert!(
        listed.contains("The ledger tests need the database up"),
        "{listed}"
    );
    // It says which session it came from.
    let store = files_under(&crystal.dir.path().join("memory"))
        .into_iter()
        .find(|file| {
            file.extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap();
    let store: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(store).unwrap()).unwrap();
    assert_eq!(store["entries"][0]["source"]["session"], "fixer");
}

#[test]
fn claude_starts_with_what_its_project_remembered_in_its_system_prompt() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "-k",
        "gotcha",
        "The ledger tests need the database up",
    ]);
    // About a file that isn't there, so stale: Claude isn't shown it.
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "-f",
        "ledger.rs",
        "Ledger rounding lives in ledger.rs",
    ]);
    let bin = fake_claude(crystal.dir.path());
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = crystal
        .command(&[
            "new",
            "-n",
            "agent",
            "-c",
            repo_dir,
            "claude",
            "fix the ledger tests",
        ])
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(out.status.success());

    let args = written(&repo.join("args"));
    assert!(
        args.contains(
            "\n\nWhat this project's earlier sessions learned:\n\
             - (gotcha) The ledger tests need the database up\n"
        ),
        "{args}"
    );
    assert!(args.contains("crystal remember"), "{args}");
    assert!(!args.contains("rounding"), "{args}");
    assert!(args.ends_with("fix the ledger tests\n"), "{args}");
}

#[test]
fn memory_search_lists_the_entries_that_share_its_words() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    for text in [
        "Fees are kept in cents",
        "The ledger tests need the database up",
        "Deploys go out on Tuesdays",
    ] {
        crystal.ok(&["remember", "-C", repo_dir, text]);
    }
    let found = crystal.ok(&["memory", "-C", repo_dir, "search", "ledger", "database"]);
    assert!(found.contains("The ledger tests"), "{found}");
    assert!(!found.contains("Fees"), "{found}");
    assert!(!found.contains("Deploys"), "{found}");
}

#[test]
fn memory_promote_adds_the_entry_to_claude_md_under_notes() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    std::fs::write(repo.join("CLAUDE.md"), "# app\n\nRun make test.\n").unwrap();
    crystal.ok(&["remember", "-C", repo_dir, "Fees are kept in cents"]);

    // Away from a terminal, it can't ask, so it needs to be told.
    let refused = crystal.fails(&["memory", "-C", repo_dir, "promote", "1"]);
    assert!(refused.contains("--yes"), "{refused}");
    crystal.ok(&["memory", "-C", repo_dir, "promote", "1", "--yes"]);
    assert_eq!(
        std::fs::read_to_string(repo.join("CLAUDE.md")).unwrap(),
        "# app\n\nRun make test.\n\n## Notes\n\n- Fees are kept in cents\n"
    );
}

#[test]
fn with_memory_off_its_commands_say_so() {
    let crystal = Crystal::new();
    let refused = crystal.fails(&["remember", "Fees are kept in cents"]);
    assert!(refused.contains("the memory plugin is off"), "{refused}");
    let refused = crystal.fails(&["memory"]);
    assert!(refused.contains("the memory plugin is off"), "{refused}");
}

#[test]
fn m_in_the_tui_shows_the_project_s_memory_and_x_forgets_an_entry() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "-k",
        "decision",
        "Fees are kept in cents",
    ]);
    crystal.ok(&["new", "-n", "agent", "-c", repo_dir, "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("▸ agent");
    tui.type_keys("m");
    tui.shows("memory · 1 entry");
    tui.shows("decision 1");
    tui.shows("from you");

    tui.type_keys("x");
    tui.shows("forget entry 1?");
    tui.type_keys("y");
    tui.shows("nothing remembered yet");
    assert_eq!(crystal.ok(&["memory", "-C", repo_dir]), "");

    tui.type_keys("\x1b");
    tui.shows("▸ agent");
}

/// Another machine for `crystal ssh` to reach: a fake ssh that runs the
/// remote command right here, as if this machine were the other one, with a
/// home and a PATH of its own.
struct Remote {
    _dir: TempDir,
    home: PathBuf,
    /// The other machine's PATH, ahead of the system's own directories.
    bin: PathBuf,
    ssh: PathBuf,
}

impl Remote {
    fn new() -> Remote {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        // ssh joins what follows the destination into one command line for
        // the other machine's shell; this one notes its arguments and runs
        // that line with `sh -c`.
        let ssh = dir.path().join("ssh");
        script(
            &ssh,
            r#"printf '%s\n' "$*" >> "$REMOTE_HOME/ssh-calls"
while [ $# -gt 0 ]; do
    case "$1" in
        --) shift; break ;;
        -*) shift ;;
        *) break ;;
    esac
done
shift
HOME="$REMOTE_HOME" PATH="$REMOTE_BIN:/usr/bin:/bin" exec sh -c "$*"
"#,
        );
        Remote {
            _dir: dir,
            home,
            bin,
            ssh,
        }
    }

    /// Puts a stand-in crystal at `path` that says it's `version`, writes
    /// down the arguments it runs with in `~/ran`, and exits 7 when the
    /// first is `fail`.
    fn put_crystal(&self, path: &Path, version: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = format!(
            r#"if [ "$1" = --version ]; then echo "crystal {version}"; exit 0; fi
printf '%s\n' "$@" > "$HOME/ran"
if [ "$1" = fail ]; then exit 7; fi
"#
        );
        script(path, &body);
    }

    fn local_bin_crystal(&self) -> PathBuf {
        self.home.join(".local/bin/crystal")
    }

    /// Runs `crystal ssh <args>` through the fake ssh.
    fn ssh(&self, crystal: &Crystal, args: &[&str]) -> Output {
        let mut all = vec!["ssh"];
        all.extend_from_slice(args);
        crystal
            .command(&all)
            .env("CRYSTAL_SSH", &self.ssh)
            .env("REMOTE_HOME", &self.home)
            .env("REMOTE_BIN", &self.bin)
            .output()
            .unwrap()
    }

    fn file(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.home.join(name)).ok()
    }
}

/// Writes an executable shell script.
fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

const OUR_VERSION: &str = env!("CARGO_PKG_VERSION");

#[test]
fn ssh_runs_a_crystal_command_on_the_other_machine() {
    let crystal = Crystal::new();
    let remote = Remote::new();
    remote.put_crystal(&remote.bin.join("crystal"), OUR_VERSION);

    let out = remote.ssh(&crystal, &["box", "ls", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(remote.file("ran").unwrap(), "ls\n--json\n");
    // Not at a terminal, so no terminal is asked for over there.
    // Two calls, one to find crystal and one to run it; the first carries a
    // script of several lines.
    let calls = remote.file("ssh-calls").unwrap();
    let starts = calls.lines().filter(|line| line.starts_with("-- box "));
    assert_eq!(starts.count(), 2, "{calls}");
    assert!(
        !calls.lines().any(|line| line.starts_with("-t ")),
        "{calls}"
    );
}

#[test]
fn ssh_finds_crystal_where_the_install_script_puts_it() {
    let crystal = Crystal::new();
    let remote = Remote::new();
    // The Crystal language's `crystal` on the PATH isn't ours.
    script(
        &remote.bin.join("crystal"),
        "echo 'Crystal 1.11.2 [LLVM 15]'\n",
    );
    remote.put_crystal(&remote.local_bin_crystal(), OUR_VERSION);

    let out = remote.ssh(&crystal, &["box", "ls"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(remote.file("ran").unwrap(), "ls\n");
}

#[test]
fn ssh_hands_over_arguments_exactly_as_typed() {
    let crystal = Crystal::new();
    let remote = Remote::new();
    remote.put_crystal(&remote.bin.join("crystal"), OUR_VERSION);

    let prompt = r#"it's $HOME and "quoted"; rm -rf ~"#;
    let out = remote.ssh(&crystal, &["box", "new", "-d", "claude", prompt]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        remote.file("ran").unwrap(),
        format!("new\n-d\nclaude\n{prompt}\n")
    );
}

#[test]
fn ssh_exits_the_way_the_command_over_there_did() {
    let crystal = Crystal::new();
    let remote = Remote::new();
    remote.put_crystal(&remote.bin.join("crystal"), OUR_VERSION);

    let out = remote.ssh(&crystal, &["box", "fail"]);
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn ssh_wont_install_crystal_without_being_asked_to() {
    let crystal = Crystal::new();
    let remote = Remote::new();

    let out = remote.ssh(&crystal, &["box", "ls"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("isn't installed on box"), "{err}");
    assert!(err.contains("crystal ssh --install box"), "{err}");
    assert_eq!(remote.file("curl-calls"), None);
}

#[test]
fn ssh_install_runs_the_install_script_there_then_the_command() {
    let crystal = Crystal::new();
    let remote = Remote::new();
    // A curl that hands out an "install script" putting a stand-in crystal
    // where the real one goes; nothing is downloaded.
    remote.put_crystal(&remote.bin.join("to-install"), OUR_VERSION);
    script(
        &remote.bin.join("curl"),
        r#"echo "$*" >> "$HOME/curl-calls"
echo 'mkdir -p "$HOME/.local/bin" && cp "$REMOTE_BIN/to-install" "$HOME/.local/bin/crystal"'
"#,
    );

    let out = remote.ssh(&crystal, &["--install", "box", "ls"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(remote.file("curl-calls").unwrap().contains("install.sh"));
    assert_eq!(remote.file("ran").unwrap(), "ls\n");
}

#[test]
fn ssh_says_when_crystal_over_there_is_another_version() {
    let crystal = Crystal::new();
    let remote = Remote::new();
    remote.put_crystal(&remote.bin.join("crystal"), "0.0.1");

    let out = remote.ssh(&crystal, &["box", "ls"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("crystal on box is 0.0.1") && err.contains(OUR_VERSION),
        "{err}"
    );
    // Not asked to upgrade, and no one at a terminal to ask: it runs as is.
    assert_eq!(remote.file("curl-calls"), None);
    assert_eq!(remote.file("ran").unwrap(), "ls\n");
}

#[test]
fn ssh_uses_the_ssh_on_the_path() {
    let crystal = Crystal::new();
    let remote = Remote::new();
    remote.put_crystal(&remote.bin.join("crystal"), OUR_VERSION);
    let path_dir = tempfile::tempdir().unwrap();
    std::fs::copy(&remote.ssh, path_dir.path().join("ssh")).unwrap();
    let path = format!(
        "{}:{}",
        path_dir.path().display(),
        std::env::var("PATH").unwrap()
    );

    let out = crystal
        .command(&["ssh", "box", "ls"])
        .env_remove("CRYSTAL_SSH")
        .env("PATH", path)
        .env("REMOTE_HOME", &remote.home)
        .env("REMOTE_BIN", &remote.bin)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(remote.file("ran").unwrap(), "ls\n");
}

/// A stand-in for Codex: a `codex` that writes down the arguments it was
/// started with, one per line, and for a new conversation writes its
/// rollout the way Codex does, into `$CODEX_HOME/sessions/YYYY/MM/DD/`,
/// named after when it started and `$FAKE_CODEX_ID`. Like Codex, it notes
/// the time it starts but writes the file later: once `$FAKE_CODEX_WAIT_FOR`
/// exists, if that's set, the way Codex waits for a first prompt. Then it
/// waits. `codex debug models` lists two models, one of them hidden, and
/// is otherwise left out of all that. Returns the directory to put on the
/// PATH.
fn fake_codex(dir: &Path) -> PathBuf {
    let bin = dir.join("codex-bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    let script = r#"#!/bin/sh
if [ "$1" = debug ]; then
    echo '{"models": [{"slug": "gpt-test-mini", "visibility": "list"},'
    echo '            {"slug": "gpt-internal", "visibility": "hide"}]}'
    exit 0
fi
printf '%s\n' "$@" > args.new && mv args.new args
if [ "$1" != resume ]; then
    dir="$CODEX_HOME/sessions/$(date +%Y/%m/%d)"
    stamp=$(date +%Y-%m-%dT%H-%M-%S)
    started="$(date -u +%Y-%m-%dT%H:%M:%S).000Z"
    if [ -n "$FAKE_CODEX_WAIT_FOR" ]; then
        while [ ! -e "$FAKE_CODEX_WAIT_FOR" ]; do sleep 0.05; done
    fi
    mkdir -p "$dir"
    printf '{"timestamp":"%s","type":"session_meta","payload":{"id":"%s","timestamp":"%s","cwd":"%s"}}\n' \
        "$started" "$FAKE_CODEX_ID" "$started" "$(pwd -P)" \
        > "$dir/rollout-$stamp-$FAKE_CODEX_ID.jsonl"
fi
sleep 30
"#;
    std::fs::write(&codex, script).unwrap();
    let mut permissions = std::fs::metadata(&codex).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&codex, permissions).unwrap();
    bin
}

/// Runs crystal with the fake Codex in `bin` on the PATH, a Codex home in
/// the test's directory, and `id` for the conversation it starts.
fn with_codex(crystal: &Crystal, bin: &Path, id: &str, args: &[&str]) {
    let out = codex_command(crystal, bin, id, args).output().unwrap();
    assert!(
        out.status.success(),
        "crystal {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn codex_command(crystal: &Crystal, bin: &Path, id: &str, args: &[&str]) -> Command {
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut command = crystal.command(args);
    command
        .env("PATH", path)
        .env("CODEX_HOME", crystal.dir.path().join("codex-home"))
        .env("FAKE_CODEX_ID", id);
    command
}

/// The arguments the fake Codex was last started with, once it has been.
fn codex_args(crystal: &Crystal) -> Vec<String> {
    let args = written(&crystal.dir.path().join("args"));
    args.lines().map(String::from).collect()
}

fn forget_codex_args(crystal: &Crystal) {
    std::fs::remove_file(crystal.dir.path().join("args")).unwrap();
}

/// Every file under `dir`, however deep.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else {
            files.push(path);
        }
    }
    files
}

const CODEX_TASK: &[&str] = &[
    "new",
    "-n",
    "coder",
    "codex",
    "--model",
    "o4",
    "fix the tests",
];

#[test]
fn codex_picks_its_conversation_up_again_after_a_restart() {
    let crystal = Crystal::new();
    let bin = fake_codex(crystal.dir.path());
    let daemon = crystal.start_daemon();
    with_codex(&crystal, &bin, "thread-123", CODEX_TASK);
    assert_eq!(codex_args(&crystal), ["--model", "o4", "fix the tests"]);
    eventually("the conversation is found and saved", || {
        crystal.saved().contains("thread-123")
    });

    crash(daemon);
    forget_codex_args(&crystal);
    // The next command starts a daemon, which starts coder again: in its
    // conversation, with its options, and without its first prompt again.
    with_codex(
        &crystal,
        &bin,
        "thread-456",
        &["new", "-n", "other", "sleep", "300"],
    );
    assert_eq!(
        codex_args(&crystal),
        ["resume", "thread-123", "--model", "o4"]
    );
}

#[test]
fn a_respawned_codex_picks_its_conversation_up_again() {
    let crystal = Crystal::new();
    let bin = fake_codex(crystal.dir.path());
    with_codex(&crystal, &bin, "thread-123", CODEX_TASK);
    eventually("the conversation is found and saved", || {
        crystal.saved().contains("thread-123")
    });

    let pid = crystal.pid("coder");
    // SAFETY: kill only sends a signal, to the group the program leads.
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    eventually("coder has ended", || {
        crystal.row("coder").unwrap()[1].starts_with("killed")
    });

    forget_codex_args(&crystal);
    with_codex(&crystal, &bin, "thread-456", &["respawn", "coder"]);
    assert_eq!(
        codex_args(&crystal),
        ["resume", "thread-123", "--model", "o4"]
    );
}

#[test]
fn codex_starts_afresh_when_its_rollout_is_gone() {
    let crystal = Crystal::new();
    let bin = fake_codex(crystal.dir.path());
    let daemon = crystal.start_daemon();
    with_codex(&crystal, &bin, "thread-123", CODEX_TASK);
    eventually("the conversation is found and saved", || {
        crystal.saved().contains("thread-123")
    });

    crash(daemon);
    for rollout in files_under(&crystal.dir.path().join("codex-home")) {
        std::fs::remove_file(rollout).unwrap();
    }
    forget_codex_args(&crystal);
    with_codex(
        &crystal,
        &bin,
        "thread-456",
        &["new", "-n", "other", "sleep", "300"],
    );

    // Started as it was asked for, in a new conversation, which crystal
    // finds in turn.
    assert_eq!(codex_args(&crystal), ["--model", "o4", "fix the tests"]);
    eventually("the new conversation is found and saved", || {
        crystal.saved().contains("thread-456")
    });
}

#[test]
fn two_codex_sessions_in_one_directory_each_find_their_own_conversation() {
    let crystal = Crystal::new();
    let bin = fake_codex(crystal.dir.path());
    // The first is started first but sent its first prompt last, so its
    // rollout appears after the second's.
    let go_first = crystal.dir.path().join("go-first");
    let out = codex_command(
        &crystal,
        &bin,
        "first-thread",
        &["new", "-n", "first", "codex"],
    )
    .env("FAKE_CODEX_WAIT_FOR", &go_first)
    .output()
    .unwrap();
    assert!(out.status.success());
    // Seconds apart, as the rollouts' names count them.
    thread::sleep(Duration::from_secs(3));
    with_codex(
        &crystal,
        &bin,
        "second-thread",
        &["new", "-n", "second", "codex"],
    );

    eventually("the second's conversation is found", || {
        crystal.saved().contains("second-thread")
    });
    // Time for the first to look too, while only the second's rollout is
    // there to find.
    thread::sleep(Duration::from_millis(1500));
    std::fs::write(&go_first, "").unwrap();
    eventually("both conversations are found", || {
        let saved = crystal.saved();
        saved.contains("first-thread") && saved.contains("second-thread")
    });
    let saved: serde_json::Value = serde_json::from_str(&crystal.saved()).unwrap();
    let conversation_of = |name: &str| {
        let sessions = saved.as_array().unwrap();
        let session = sessions.iter().find(|s| s["name"] == name).unwrap();
        session["conversation"]["id"].as_str().unwrap().to_string()
    };
    assert_eq!(conversation_of("first"), "first-thread");
    assert_eq!(conversation_of("second"), "second-thread");
}

/// A stand-in for `claude -p`. Each run notes its arguments in `runs`, one
/// line each, then writes what a short run of Claude writes as stream-json:
/// the conversation, some text, a tool and its answer. It waits for the
/// test to make `finish-<run>` before its answer and result. With
/// `FAKE_FAIL` set its result says it failed; with `FAKE_CRASH` it writes an
/// error and exits before saying anything. Returns the directory to put on
/// the PATH.
fn print_claude(dir: &Path) -> PathBuf {
    let bin = dir.join("print-bin");
    std::fs::create_dir(&bin).unwrap();
    let claude = bin.join("claude");
    let script = r#"#!/bin/sh
echo "$*" >> runs
run=$(wc -l < runs | tr -d ' ')
if [ -n "$FAKE_CRASH" ]; then
    echo 'Error: Invalid API key' >&2
    exit 1
fi
echo '{"type":"system","subtype":"init","session_id":"conv-1","cwd":"/x","model":"m"}'
echo '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Looking at the tests."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}'
echo '{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"test result: ok. 3 passed","is_error":false}]}}'
while [ ! -e "finish-$run" ]; do sleep 0.05; done
echo '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"All green on run '"$run"'."}]}}'
if [ -n "$FAKE_FAIL" ]; then
    echo '{"type":"result","subtype":"error_max_turns","is_error":true,"session_id":"conv-1","total_cost_usd":0.01,"duration_ms":900}'
    exit 1
fi
echo '{"type":"result","subtype":"success","is_error":false,"result":"All green on run '"$run"'.","session_id":"conv-1","total_cost_usd":0.0421,"duration_ms":3200,"permission_denials":[{"tool_name":"Bash","tool_use_id":"t9","tool_input":{"command":"rm -rf build"}}]}'
"#;
    std::fs::write(&claude, script).unwrap();
    let mut permissions = std::fs::metadata(&claude).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&claude, permissions).unwrap();
    bin
}

/// The PATH, with `bin` ahead of the rest.
fn path_with(bin: &Path) -> String {
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

/// The arguments of each `claude -p` run so far, once there are `count`.
fn runs(dir: &Path, count: usize) -> Vec<String> {
    let file = dir.join("runs");
    eventually(&format!("claude has run {count} times"), || {
        std::fs::read_to_string(&file).is_ok_and(|runs| runs.lines().count() == count)
    });
    let runs = std::fs::read_to_string(&file).unwrap();
    runs.lines().map(String::from).collect()
}

fn finish_run(dir: &Path, run: usize) {
    std::fs::write(dir.join(format!("finish-{run}")), "").unwrap();
}

fn status(crystal: &Crystal, name: &str) -> String {
    crystal.row(name).unwrap()[1].clone()
}

#[test]
fn a_task_runs_claude_without_a_terminal_and_shows_what_it_did() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    let out = crystal
        .command(&["task", "-n", "fixer", "fix", "the", "tests"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "fixer\n");

    // Claude was asked for its events, with the prompt behind `--`.
    assert_eq!(
        runs(dir, 1),
        ["-p --output-format stream-json --verbose -- fix the tests"]
    );
    eventually("the task is working", || {
        status(&crystal, "fixer") == "working"
    });
    shows_on_screen(&crystal, "fixer", "> fix the tests");
    shows_on_screen(&crystal, "fixer", "▸ Bash cargo test");
    shows_on_screen(&crystal, "fixer", "└ test result: ok. 3 passed");

    finish_run(dir, 1);
    eventually("the task is done", || status(&crystal, "fixer") == "done");
    shows_on_screen(&crystal, "fixer", "All green on run 1.");
    shows_on_screen(&crystal, "fixer", "✓ done");
    shows_on_screen(&crystal, "fixer", "$0.04");
    shows_on_screen(&crystal, "fixer", "refused: Bash rm -rf build");
    assert_eq!(
        crystal.row("fixer").unwrap()[7],
        "claude -p 'fix the tests'"
    );

    assert_eq!(crystal.ok(&["result", "fixer"]), "All green on run 1.\n");
    let result: serde_json::Value =
        serde_json::from_str(&crystal.ok(&["result", "fixer", "--json"])).unwrap();
    assert_eq!(result["failed"], false);
    assert_eq!(result["conversation"], "conv-1");
    assert_eq!(result["cost_usd"], 0.0421);
    assert_eq!(result["runs"], 1);
}

#[test]
fn a_follow_up_carries_the_conversation_on_one_run_at_a_time() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    let start = crystal
        .command(&[
            "task",
            "-n",
            "fixer",
            "fix the tests",
            "--",
            "--model",
            "opus",
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(start.status.success());
    finish_run(dir, 1);
    eventually("the task is done", || status(&crystal, "fixer") == "done");

    crystal.ok(&["send", "fixer", "now", "the", "docs"]);
    let runs = runs(dir, 2);
    assert_eq!(
        runs[1],
        "-p --output-format stream-json --verbose --resume conv-1 --model opus -- now the docs"
    );
    eventually("the follow-up is working", || {
        status(&crystal, "fixer") == "working"
    });
    let busy = crystal.fails(&["send", "fixer", "and the changelog"]);
    assert!(busy.contains("still working"), "{busy}");
    assert!(
        crystal
            .fails(&["result", "fixer"])
            .contains("still working")
    );
    let keys = crystal.fails(&["send-keys", "fixer", "Enter"]);
    assert!(keys.contains("takes no keys"), "{keys}");

    finish_run(dir, 2);
    eventually("the follow-up is done", || {
        status(&crystal, "fixer") == "done"
    });
    shows_on_screen(&crystal, "fixer", "> now the docs");
    assert_eq!(crystal.ok(&["result", "fixer"]), "All green on run 2.\n");
}

#[test]
fn task_wait_waits_for_the_run_and_says_how_it_ended() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    finish_run(dir, 1);
    let out = crystal
        .command(&["task", "--wait", "-n", "quick", "fix the tests"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "quick\ndone\n");
}

#[test]
fn a_task_whose_run_fails_ends_and_says_why() {
    let crystal = Crystal::new();
    let path = path_with(&print_claude(crystal.dir.path()));
    let start = |name: &str, fault: &str| {
        let dir = crystal.dir.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        let out = crystal
            .command(&[
                "task",
                "-n",
                name,
                "-c",
                dir.to_str().unwrap(),
                "fix the tests",
            ])
            .env("PATH", &path)
            .env(fault, "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        dir
    };

    // Claude says the run failed.
    let failing = start("failing", "FAKE_FAIL");
    finish_run(&failing, 1);
    eventually("the task has ended", || {
        status(&crystal, "failing") == "exited 1"
    });
    shows_on_screen(&crystal, "failing", "✗ failed · error max turns");
    let result: serde_json::Value =
        serde_json::from_str(&crystal.ok(&["result", "failing", "--json"])).unwrap();
    assert_eq!(result["failed"], true);

    // Claude crashes before it says anything.
    start("crashing", "FAKE_CRASH");
    eventually("the task has ended", || {
        status(&crystal, "crashing") == "exited 1"
    });
    shows_on_screen(&crystal, "crashing", "Error: Invalid API key");
    assert_eq!(
        crystal.ok(&["result", "crashing"]),
        "Error: Invalid API key\n"
    );
    assert!(
        crystal
            .fails(&["send", "crashing", "again"])
            .contains("has ended")
    );
}

#[test]
fn a_task_comes_back_at_rest_after_a_restart_and_carries_its_conversation_on() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    let daemon = crystal.start_daemon();
    let out = crystal
        .command(&["task", "-n", "fixer", "fix the tests"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    finish_run(dir, 1);
    eventually("the task is done", || status(&crystal, "fixer") == "done");
    eventually("its conversation is saved", || {
        crystal.saved().contains("conv-1")
    });

    crash(daemon);
    // The next command starts a daemon, which brings the task back at rest
    // rather than running its prompt again.
    let out = crystal
        .command(&["new", "-n", "other", "sleep", "30"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    eventually("the task is back", || {
        crystal.row("fixer").is_some_and(|row| row[1] == "idle")
    });
    shows_on_screen(&crystal, "fixer", "crystal restarted");
    assert_eq!(runs(dir, 1).len(), 1);

    crystal.ok(&["send", "fixer", "carry on"]);
    assert_eq!(
        runs(dir, 2)[1],
        "-p --output-format stream-json --verbose --resume conv-1 -- carry on"
    );
}

#[test]
fn a_relative_socket_path_names_the_same_socket_for_the_daemon() {
    let crystal = Crystal::new();
    let mut new = Command::new(CRYSTAL);
    new.args([
        "--socket",
        "relative.sock",
        "new",
        "-n",
        "here",
        "sleep",
        "30",
    ])
    .current_dir(crystal.dir.path())
    .env("XDG_CONFIG_HOME", crystal.dir.path());
    let out = new.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(crystal.dir.path().join("relative.sock").exists());

    let mut stop = Command::new(CRYSTAL);
    stop.args(["--socket", "relative.sock", "kill-server"])
        .current_dir(crystal.dir.path());
    assert!(stop.output().unwrap().status.success());
}

#[test]
fn slash_filters_the_sidebar_and_enter_selects_the_match() {
    let crystal = Crystal::new();
    for name in ["planner", "refund-fix", "reviewer"] {
        let script = format!("echo {name} is here; echo > {name}-ready; sleep 30");
        crystal.ok(&["new", "-n", name, "sh", "-c", &script]);
        written(&crystal.dir.path().join(format!("{name}-ready")));
    }
    let mut tui = crystal.tui();
    tui.shows("planner is here");

    tui.type_keys("/fix");
    tui.shows("find: fix");
    tui.shows("1 match");
    eventually("only the match is in the sidebar", || {
        let sidebar = sidebar_of(&tui.text());
        sidebar.contains("refund-fix") && !sidebar.contains("planner")
    });
    assert!(tui.text().contains("planner is here"), "not selected yet");

    tui.type_keys("\r");
    tui.shows("refund-fix is here");
    tui.shows("❯ planner");
}

/// A stand-in for GitHub's `gh`: it answers `pr list` and `issue list` with
/// the JSON given, `issue view` with an issue's text, and writes each call
/// it gets into `gh-calls` beside it. Returns the directory to put first on
/// the PATH.
fn fake_gh(dir: &Path, pull_requests: &str, issues: &str) -> PathBuf {
    let bin = dir.join("gh-bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(dir.join("gh-prs.json"), pull_requests).unwrap();
    std::fs::write(dir.join("gh-issues.json"), issues).unwrap();
    let dir = dir.display();
    script(
        &bin.join("gh"),
        &format!(
            r#"echo "$*" >> "{dir}/gh-calls"
case "$1 $2" in
    "pr list") cat "{dir}/gh-prs.json" ;;
    "pr view") ;;
    "issue list") cat "{dir}/gh-issues.json" ;;
    "issue view") echo '{{"body": "The login page sends you back to itself."}}' ;;
    *) echo "unexpected: $*" >&2; exit 1 ;;
esac
"#
        ),
    );
    bin
}

/// A repository called `app` whose origin is on github.com, as far as git
/// can tell.
fn github_repo(dir: &Path) -> PathBuf {
    let repo = git_repo(dir, "app");
    git(
        &repo,
        &["remote", "add", "origin", "https://github.com/acme/app.git"],
    );
    repo
}

const NO_ISSUES: &str = "[]";

#[test]
fn a_worktree_shows_its_pull_request_and_o_opens_it() {
    let crystal = Crystal::new();
    let repo = github_repo(crystal.dir.path());
    let failing = r#"[{"number": 57, "title": "Fix the login redirect",
        "headRefName": "fix-login", "isDraft": false, "reviewDecision": "",
        "statusCheckRollup": [{"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "FAILURE"}],
        "url": "https://github.com/acme/app/pull/57"}]"#;
    let bin = fake_gh(crystal.dir.path(), failing, NO_ISSUES);
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&[
        "new",
        "-n",
        "fixer",
        "-c",
        repo_arg,
        "-w",
        "fix-login",
        "sleep",
        "30",
    ]);

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("#57 ✗");

    tui.type_keys("o");
    let calls = crystal.dir.path().join("gh-calls");
    eventually("gh is asked to open the pull request", || {
        let calls = std::fs::read_to_string(&calls).unwrap_or_default();
        calls.lines().any(|call| call == "pr view --web 57")
    });
}

#[test]
fn o_says_so_when_the_branch_has_no_pull_request() {
    let crystal = Crystal::new();
    let repo = github_repo(crystal.dir.path());
    let bin = fake_gh(crystal.dir.path(), "[]", NO_ISSUES);
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("▸ planner");
    let calls = crystal.dir.path().join("gh-calls");
    eventually("gh has been asked about pull requests", || {
        std::fs::read_to_string(&calls).is_ok_and(|calls| calls.contains("pr list"))
    });
    // The answer may still be on its way; the notice waits for a key.
    eventually("the footer says there's none", || {
        tui.type_keys("o");
        thread::sleep(Duration::from_millis(100));
        tui.text().contains("no open pull request for main")
    });
}

#[test]
fn i_lists_the_issues_and_enter_starts_a_session_for_one() {
    let crystal = Crystal::new();
    let repo = github_repo(crystal.dir.path());
    let issues = r#"[
        {"number": 7, "title": "Dark mode", "labels": [{"name": "idea"}],
         "updatedAt": "2026-09-01T10:00:00Z", "author": {"login": "bo"},
         "url": "https://github.com/acme/app/issues/7"},
        {"number": 42, "title": "Fix login redirect", "labels": [{"name": "bug"}],
         "updatedAt": "2026-10-01T10:00:00Z", "author": {"login": "ana"},
         "url": "https://github.com/acme/app/issues/42"}
    ]"#;
    let bin = fake_gh(crystal.dir.path(), "[]", issues);
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);

    let claude = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin, &claude]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("▸ planner");
    tui.type_keys("i");
    tui.shows("issues · app");
    // The latest to change comes first, its text under the list.
    tui.shows("Fix login redirect");
    tui.shows("The login page sends you back to itself.");
    let text = tui.text();
    assert!(line_with(&text, "#42") < line_with(&text, "#7"), "{text}");

    tui.type_keys("dark");
    tui.hides("Fix login redirect");
    tui.shows("Dark mode");
    tui.type_keys("\x7f\x7f\x7f\x7f");
    tui.shows("Fix login redirect");
    // The bar stayed on #7 while it was shown; up goes back to #42.
    tui.type_keys("\x1b[A");

    tui.type_keys("\r");
    tui.shows("New session · app ⎇ 42-fix-login-redirect");
    tui.shows("Fix issue #42: Fix login redirect");
    tui.type_keys("\r");
    tui.shows("⎇ 42-fix-login-redirect");

    let worktree = crystal
        .dir
        .path()
        .join("app.worktrees/42-fix-login-redirect");
    let args = written(&worktree.join("args"));
    assert_eq!(
        args.lines().last(),
        Some("Fix issue #42: Fix login redirect (https://github.com/acme/app/issues/42)")
    );
}

/// A repository with work in it, the way an agent leaves one: on `main`, a
/// refund function and some notes; on the branch `fee`, one commit that
/// changes the refund; and, not committed yet, a line added to the notes
/// and a new file git doesn't know about.
fn repo_with_work(dir: &Path) -> PathBuf {
    let repo = git_repo(dir, "app");
    std::fs::write(
        repo.join("refund.rs"),
        "fn refund(order: &Order) {\n    let total = order.total;\n    ledger.write(total);\n}\n",
    )
    .unwrap();
    std::fs::write(repo.join("notes.md"), "# Notes\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "refunds"]);

    git(&repo, &["checkout", "-q", "-b", "fee"]);
    std::fs::write(
        repo.join("refund.rs"),
        "fn refund(order: &Order) {\n    let total = order.total - order.fee;\n    ledger.write(total);\n}\n",
    )
    .unwrap();
    git(&repo, &["commit", "-q", "-am", "take the fee off"]);

    std::fs::write(repo.join("notes.md"), "# Notes\n- ask about fees\n").unwrap();
    std::fs::write(repo.join("todo.txt"), "write the tests\n").unwrap();
    repo
}

#[test]
fn d_shows_what_changed_in_the_selected_sessions_worktree() {
    let crystal = Crystal::new();
    let repo = repo_with_work(crystal.dir.path());
    crystal.ok(&[
        "new",
        "-n",
        "agent",
        "-c",
        repo.to_str().unwrap(),
        "sleep",
        "30",
    ]);

    let mut tui = crystal.tui();
    tui.shows("agent");
    tui.type_keys("d");
    tui.shows("uncommitted changes · 2 files +2 −0");
    // The notes' new line, and the file git doesn't know about yet.
    tui.shows("notes.md");
    tui.shows("todo.txt");
    tui.shows("+ - ask about fees");

    // The branch, the way its pull request would read.
    tui.type_keys("b");
    tui.shows("the branch since main · 1 file +1 −1");
    tui.shows("refund.rs");
    tui.shows("order.fee");
    tui.hides("todo.txt");

    tui.type_keys("\x1b");
    tui.hides("the branch since main");
    tui.shows("? keys");
}

#[test]
fn v_puts_the_old_line_beside_its_new_version() {
    let crystal = Crystal::new();
    let repo = repo_with_work(crystal.dir.path());
    crystal.ok(&[
        "new",
        "-n",
        "agent",
        "-c",
        repo.to_str().unwrap(),
        "sleep",
        "30",
    ]);

    let mut tui = crystal.tui();
    tui.shows("agent");
    tui.type_keys("d");
    tui.shows("uncommitted changes");
    tui.type_keys("b");
    tui.shows("refund.rs");
    // 80 columns is too narrow for two files side by side.
    tui.type_keys("v");
    tui.shows("too narrow for side by side");

    // Beside the list of files, 180 columns leave room.
    tui.resize(30, 180);
    tui.hides("too narrow for side by side");
    eventually("the old and new lines share a row", || {
        tui.text()
            .lines()
            .any(|row| row.contains("order.total;") && row.contains("order.fee;"))
    });
}

#[test]
fn p_finds_a_file_and_opens_it_in_the_editor() {
    let crystal = Crystal::new();
    let repo = repo_with_work(crystal.dir.path());
    crystal.ok(&[
        "new",
        "-n",
        "agent",
        "-c",
        repo.to_str().unwrap(),
        "sleep",
        "30",
    ]);
    // An editor that notes the file it was asked to open.
    let editor = crystal.dir.path().join("editor");
    let edited = crystal.dir.path().join("edited");
    script(&editor, "printf '%s\\n' \"$1\" > \"$EDITED\"\nsleep 30\n");

    let mut tui = crystal.attach_with_env(
        &[],
        &[
            ("EDITOR", editor.to_str().unwrap()),
            ("EDITED", edited.to_str().unwrap()),
        ],
    );
    tui.shows("agent");
    tui.type_keys("p");
    tui.shows("find a file · 3 files");
    tui.type_keys("rfnd");
    // The match, and the start of the file beside it.
    tui.shows("refund.rs");
    tui.shows("fn refund(order: &Order) {");
    tui.hides("notes.md");

    tui.type_keys("\r");
    assert_eq!(written(&edited), "refund.rs\n");
    tui.shows("typing into refund.rs");
}

#[test]
fn what_is_in_front_follows_a_shell_and_the_agent_it_runs() {
    let crystal = Crystal::new();
    // A pretend Claude Code that stays in front until the test creates
    // `quit`.
    let bin = crystal.dir.path().join("agent-bin");
    std::fs::create_dir_all(&bin).unwrap();
    script(
        &bin.join("claude"),
        "echo pretend claude; while [ ! -e quit ]; do sleep 0.05; done\n",
    );
    let out = crystal
        .command(&["new", "-n", "box", "sh"])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(out.status.success());
    let program = || crystal.row("box").map(|row| row[6].clone());
    eventually("the shell is in front", || {
        program().as_deref() == Some("sh")
    });

    crystal.ok(&["send", "box", "claude"]);
    eventually("claude is in front", || {
        program().as_deref() == Some("claude")
    });
    let json = crystal.ok(&["ls", "--json"]);
    assert!(json.contains(r#""kind": "agent""#), "{json}");
    assert!(json.contains(r#""name": "Claude Code""#), "{json}");

    // What Claude says it's doing holds while it's in front…
    let hook = format!("{CRYSTAL} hook claude");
    run_hook(
        &crystal,
        "box",
        &hook,
        r#"{"hook_event_name":"UserPromptSubmit"}"#,
    );
    assert_eq!(crystal.row("box").unwrap()[1], "working");

    // …and goes with it when the shell is back in front.
    std::fs::write(crystal.dir.path().join("quit"), "").unwrap();
    eventually("the shell is back in front", || {
        program().as_deref() == Some("sh")
    });
    assert_eq!(crystal.row("box").unwrap()[1], "running");
}

#[test]
fn a_shell_never_looks_like_an_agent_whatever_it_prints() {
    let crystal = Crystal::new();
    // What an agent draws while it waits on the user, printed by a shell.
    let script = "printf 'Do you want to proceed?'; sleep 30";
    crystal.ok(&["new", "-n", "plain", "sh", "-c", script]);
    eventually("the shell is in front", || {
        crystal.row("plain").map(|row| row[6].clone()).as_deref() == Some("sh")
    });
    thread::sleep(Duration::from_millis(800));
    assert_eq!(crystal.row("plain").unwrap()[1], "running");
}

/// A stand-in for Claude Code that does its task: it writes down its
/// arguments, waits for the test to make the file `$FINISH`, then closes
/// its task the way it's told to, with `crystal done`, and waits. Returns
/// the directory to put on the PATH.
fn finishing_claude(dir: &Path) -> PathBuf {
    let bin = dir.join("finishing-bin");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join("claude"),
        &format!(
            "printf '%s\\n' \"$@\" > args.new && mv args.new args\n\
             while [ ! -e \"$FINISH\" ]; do sleep 0.05; done\n\
             {CRYSTAL} done \"did what was asked\"\n\
             sleep 30\n"
        ),
    );
    bin
}

/// The `ls --json` entry for `name`.
fn listed(crystal: &Crystal, name: &str) -> serde_json::Value {
    let sessions: serde_json::Value = serde_json::from_str(&crystal.ok(&["ls", "--json"])).unwrap();
    let session = sessions
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name);
    session.unwrap().clone()
}

#[test]
fn an_agent_closes_its_task_with_crystal_done_and_ls_shows_how_it_went() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let bin = finishing_claude(dir);
    let finish = dir.join("finish");
    let out = crystal
        .command(&["new", "-d", "-n", "fixer", "claude", "fix the tests"])
        .env("PATH", path_of(&[&bin]))
        .env("FINISH", &finish)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Claude is told how to close its task, and the task is open.
    let args = written(&dir.join("args"));
    assert!(args.contains("crystal done"), "{args}");
    assert!(args.ends_with("fix the tests\n"), "{args}");
    assert_eq!(crystal.row("fixer").unwrap()[8], "fix the tests");
    assert_eq!(listed(&crystal, "fixer")["task"]["goal"], "fix the tests");

    std::fs::write(&finish, "").unwrap();
    eventually("the task is closed", || {
        crystal.row("fixer").unwrap()[8] == "✓ did what was asked"
    });
    let task = &listed(&crystal, "fixer")["task"];
    assert_eq!(task["outcome"]["failed"], false);
    assert_eq!(task["outcome"]["summary"], "did what was asked");

    let tasks = crystal.ok(&["tasks"]);
    assert!(
        tasks.starts_with("done    fixer") && tasks.contains("fix the tests — did what was asked"),
        "{tasks}"
    );
}

#[test]
fn done_needs_a_session_with_a_task() {
    let crystal = Crystal::new();
    let outside = crystal.fails(&["done", "all of it"]);
    assert!(
        outside.contains("isn't running in a crystal session"),
        "{outside}"
    );

    crystal.ok(&["new", "-d", "-n", "plain", "sleep", "30"]);
    let no_task = crystal.fails(&["done", "-n", "plain", "all of it"]);
    assert!(no_task.contains("plain has no task"), "{no_task}");
    assert_eq!(crystal.row("plain").unwrap()[8], "-");

    // `-t` makes any session a task, and `-n` closes another's.
    crystal.ok(&["new", "-d", "-n", "given", "-t", "tidy up", "sleep", "30"]);
    crystal.ok(&["done", "-n", "given", "--failed", "no", "time"]);
    assert_eq!(crystal.row("given").unwrap()[8], "✗ no time");
    let tasks = crystal.ok(&["tasks"]);
    assert!(
        tasks.contains("failed  given") && tasks.contains("tidy up — no time"),
        "{tasks}"
    );
}

#[test]
fn an_agent_that_ends_its_turn_with_its_task_open_is_reminded_once() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let out = crystal
        .command(&["new", "-d", "-n", "agent", "claude", "fix the tests"])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(out.status.success());
    let hook = format!("{CRYSTAL} hook claude");
    let session = [("CRYSTAL_SESSION", "agent")];
    let stop = r#"{"hook_event_name":"Stop"}"#;
    run_hook(
        &crystal,
        "agent",
        &hook,
        r#"{"hook_event_name":"UserPromptSubmit"}"#,
    );

    // Its Stop hook keeps it going, told to close its task, so it's still
    // working…
    let said: serde_json::Value =
        serde_json::from_str(&hook_says(&crystal, &session, &hook, stop)).unwrap();
    assert_eq!(said["decision"], "block", "{said}");
    let reason = said["reason"].as_str().unwrap();
    assert!(reason.contains("crystal done"), "{reason}");
    assert_eq!(crystal.row("agent").unwrap()[1], "working");

    // …and once is enough: it may have its reasons to leave the task open.
    run_hook(&crystal, "agent", &hook, stop);
    assert_eq!(crystal.row("agent").unwrap()[1], "done");
    assert_eq!(crystal.row("agent").unwrap()[8], "fix the tests");
}

#[test]
fn an_agent_is_never_reminded_of_a_task_it_has_closed_or_never_had() {
    let crystal = Crystal::new();
    let hook = format!("{CRYSTAL} hook claude");
    let stop = r#"{"hook_event_name":"Stop"}"#;
    // The turn ends quietly, every time.
    let ends_quietly = |name: &str| {
        for _ in 0..2 {
            run_hook(&crystal, name, &hook, stop);
            assert_eq!(crystal.row(name).unwrap()[1], "done", "{name}");
        }
    };
    crystal.ok(&["new", "-d", "-n", "plain", "sleep", "30"]);
    ends_quietly("plain");

    crystal.ok(&["new", "-d", "-n", "closed", "-t", "tidy up", "sleep", "30"]);
    crystal.ok(&["done", "-n", "closed", "tidied"]);
    ends_quietly("closed");

    // With tasks off, an open one is left to the user.
    crystal.ok(&["new", "-d", "-n", "open", "-t", "tidy up", "sleep", "30"]);
    crystal.ok(&["plugin", "disable", "tasks"]);
    ends_quietly("open");
}

#[test]
fn a_background_task_closes_itself_with_the_first_line_of_its_answer() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    let out = crystal
        .command(&["task", "-n", "fixer", "fix the tests"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    eventually("the task is working", || {
        status(&crystal, "fixer") == "working"
    });
    assert!(crystal.ok(&["tasks"]).starts_with("open    fixer"));

    finish_run(dir, 1);
    eventually("the task has closed", || {
        crystal.row("fixer").unwrap()[8] == "✓ All green on run 1."
    });
    let tasks = crystal.ok(&["tasks"]);
    assert!(
        tasks.contains("fix the tests — All green on run 1."),
        "{tasks}"
    );
}

#[test]
fn the_backlog_numbers_items_and_ticks_them_off() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "shop");
    let repo_dir = repo.to_str().unwrap();
    let backlog = |args: &[&str]| {
        let mut all = vec!["backlog", "-C", repo_dir];
        all.extend(args);
        crystal.ok(&all)
    };
    assert_eq!(
        backlog(&["add", "write", "the", "docs", "-t", "docs"]),
        "#1\n"
    );
    assert_eq!(backlog(&["add", "fix the cart"]), "#2\n");
    assert_eq!(
        backlog(&[]),
        "#1    write the docs  #docs\n#2    fix the cart\n"
    );

    // Every worktree of the project shares its backlog.
    let worktree = crystal.dir.path().join("shop-cart");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "cart",
            worktree.to_str().unwrap(),
        ],
    );
    let from_worktree = crystal.ok(&["backlog", "-C", worktree.to_str().unwrap()]);
    assert_eq!(from_worktree, backlog(&[]));

    backlog(&["done", "1"]);
    assert_eq!(backlog(&[]), "#2    fix the cart\n");
    assert_eq!(
        backlog(&["--all"]),
        "#2    fix the cart\n#1    ✓ write the docs  #docs\n"
    );
    assert_eq!(
        backlog(&["export"]),
        "# shop backlog\n\n- [ ] fix the cart (#2)\n- [x] write the docs (#1) #docs\n"
    );
    backlog(&["reopen", "1"]);
    backlog(&["rm", "2"]);
    assert_eq!(backlog(&[]), "#1    write the docs  #docs\n");
    let missing = crystal.fails(&["backlog", "-C", repo_dir, "done", "7"]);
    assert!(
        missing.contains("there's no #7 on the backlog"),
        "{missing}"
    );
    assert_eq!(
        backlog(&["add", "after"]),
        "#3\n",
        "numbers aren't used again"
    );
}

#[test]
fn a_task_started_from_the_backlog_ticks_its_item_when_it_closes_done() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "shop");
    let repo_dir = repo.to_str().unwrap();
    crystal.ok(&["backlog", "-C", repo_dir, "add", "write the docs"]);

    let bin = finishing_claude(dir);
    let finish = dir.join("finish");
    let out = crystal
        .command(&[
            "backlog", "-C", repo_dir, "start", "1", "-w", "-d", "-n", "docs",
        ])
        .env("PATH", path_of(&[&bin]))
        .env("FINISH", &finish)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "docs\n");

    // It runs in a worktree named after the item, asked to do what it says.
    let row = crystal.row("docs").unwrap();
    assert_eq!(
        (row[3].as_str(), row[8].as_str()),
        ("shop", "write the docs")
    );
    assert!(row[4].contains("write-the-docs"), "{row:?}");
    let worktree = PathBuf::from(listed(&crystal, "docs")["cwd"].as_str().unwrap());
    let args = written(&worktree.join("args"));
    assert!(args.ends_with("write the docs\n"), "{args}");

    std::fs::write(&finish, "").unwrap();
    eventually("the item is ticked", || {
        crystal.ok(&["backlog", "-C", repo_dir]).is_empty()
    });
    assert_eq!(
        crystal.ok(&["backlog", "-C", repo_dir, "--all"]),
        "#1    ✓ write the docs\n"
    );
}

#[test]
fn c_in_the_tui_closes_the_selected_sessions_task_with_a_line_on_it() {
    let crystal = Crystal::new();
    crystal.ok(&[
        "new",
        "-d",
        "-n",
        "fixer",
        "-t",
        "fix the tests",
        "sleep",
        "30",
    ]);
    let mut tui = crystal.tui();
    // The task shows under its session and in its pane's header.
    tui.shows("task: fix the tests");

    tui.type_keys("c");
    tui.shows("close fixer's task?");
    tui.type_keys("d");
    tui.shows("done; what was done:");
    tui.type_keys("all green\r");
    tui.shows("✓ all green");
    assert_eq!(crystal.row("fixer").unwrap()[8], "✓ all green");
    let tasks = crystal.ok(&["tasks"]);
    assert!(tasks.contains("fix the tests — all green"), "{tasks}");
}

#[test]
fn b_opens_the_projects_backlog_to_add_to_and_tick_off() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "shop");
    let repo_dir = repo.to_str().unwrap();
    crystal.ok(&["backlog", "-C", repo_dir, "add", "write the docs"]);
    crystal.ok(&["new", "-d", "-n", "agent", "-c", repo_dir, "sleep", "30"]);
    let mut tui = crystal.tui();
    // The project's heading counts what's to do.
    tui.shows("1 to do");

    tui.type_keys("b");
    tui.shows("backlog · shop");
    tui.shows("write the docs");
    tui.type_keys("a");
    tui.shows("add to the backlog:");
    tui.type_keys("fix the cart\r");
    tui.shows("#2");
    tui.shows("fix the cart");

    // Space ticks off the item the bar is on, the first.
    tui.type_keys(" ");
    eventually("#1 is done", || {
        crystal.ok(&["backlog", "-C", repo_dir]) == "#2    fix the cart\n"
    });
    tui.shows("✓ write the docs");
    tui.type_keys("\x1b");
    tui.hides("backlog · shop");
}

#[test]
fn the_panel_starts_claude_in_the_background_as_a_task() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let bin = print_claude(dir);
    let path = path_of(&[&bin]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);

    tui.type_keys("n");
    tui.shows("in a terminal");
    // The task, then Tab to what runs, Tab to how, and right to the
    // background.
    tui.type_keys("fix the tests\t\t\x1b[C");
    tui.shows("New background task");
    tui.shows("runs  claude -p 'fix the tests'");
    tui.type_keys("\r");

    assert_eq!(
        runs(dir, 1),
        ["-p --output-format stream-json --verbose -- fix the tests"]
    );
    assert_eq!(crystal.row("task").unwrap()[8], "fix the tests");
    finish_run(dir, 1);
    eventually("the task has closed", || {
        crystal.row("task").unwrap()[8] == "✓ All green on run 1."
    });
}

#[test]
fn a_closed_task_is_remembered_in_its_project_s_memory() {
    let crystal = Crystal::new();
    crystal.configure("notify = false\n");
    let dir = crystal.dir.path();
    let bin = finishing_claude(dir);
    let finish = dir.join("finish");
    let out = crystal
        .command(&["new", "-d", "-n", "fixer", "claude", "fix the tests"])
        .env("PATH", path_of(&[&bin]))
        .env("FINISH", &finish)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    std::fs::write(&finish, "").unwrap();
    eventually("the task is closed", || {
        crystal.row("fixer").unwrap()[8] == "✓ did what was asked"
    });
    eventually("the outcome is remembered", || {
        crystal
            .ok(&["memory"])
            .contains("fix the tests: did what was asked")
    });
}

/// Installs a plugin by hand, as `crystal plugin new` or `install` would:
/// a directory called `name` in the test's plugins directory, with
/// `manifest` for its plugin.toml and `files` beside it.
fn plugin(crystal: &Crystal, name: &str, manifest: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = crystal.config_home().join("crystal/plugins").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
    for (file, text) in files {
        std::fs::write(dir.join(file), text).unwrap();
    }
    dir
}

#[test]
fn with_memory_switched_off_claude_isn_t_shown_what_was_remembered() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "The ledger tests need the database up",
    ]);
    assert_eq!(
        crystal.ok(&["plugin", "disable", "memory"]),
        "the memory plugin is off\n"
    );

    let refused = crystal.fails(&["remember", "-C", repo_dir, "Fees are kept in cents"]);
    assert!(
        refused.contains("turn it on with `crystal plugin enable memory`"),
        "{refused}"
    );
    let bin = fake_claude(crystal.dir.path());
    let out = crystal
        .command(&["new", "-n", "agent", "-c", repo_dir, "claude", "fix it"])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(out.status.success());
    let args = written(&repo.join("args"));
    assert!(!args.contains("earlier sessions learned"), "{args}");
    assert!(!args.contains("crystal remember"), "{args}");
    assert!(args.ends_with("fix it\n"), "{args}");
}

#[test]
fn with_github_switched_off_gh_is_never_asked_until_it_s_on_again() {
    let crystal = Crystal::new();
    crystal.configure("notify = false\n\n[plugins]\nmemory = false\ngithub = false\n");
    let repo = github_repo(crystal.dir.path());
    let bin = fake_gh(crystal.dir.path(), "[]", NO_ISSUES);
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("▸ planner");
    tui.type_keys("o");
    tui.shows("the github plugin is off");
    tui.type_keys("i");
    tui.shows("the github plugin is off");
    let calls = crystal.dir.path().join("gh-calls");
    assert!(
        !calls.exists(),
        "gh was asked: {:?}",
        std::fs::read_to_string(&calls)
    );

    // Switched on in the plugins view, it's asked straight away.
    tui.type_keys("X");
    tui.shows("crystal's own");
    tui.type_keys("jjjj ");
    tui.shows("● github");
    eventually("gh is asked about pull requests", || {
        std::fs::read_to_string(&calls).is_ok_and(|calls| calls.contains("pr list"))
    });
    let config = std::fs::read_to_string(crystal.config_file()).unwrap();
    assert!(config.contains("github = true"), "{config}");
}

#[test]
fn the_keys_of_a_plugin_that_s_off_aren_t_listed() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);
    let mut tui = crystal.tui();
    tui.shows("❯ agent");
    tui.type_keys("?");
    tui.shows("the project's backlog");
    assert!(
        !tui.text().contains("what it has remembered"),
        "{}",
        tui.text()
    );
    tui.type_keys("m");
    tui.hides("the project's backlog");
    tui.type_keys("m");
    tui.shows("the memory plugin is off");
}

#[test]
fn an_event_plugin_hears_a_session_come_to_wait_once() {
    let crystal = Crystal::new();
    let manifest = r#"
name = "listener"
version = "1.0.0"

[[events]]
on = "session.*"
command = ["sh", "hook.sh"]
"#;
    let hook = r#"{ printf '%s %s ' "$CRYSTAL_EVENT" "$CRYSTAL_SESSION"; cat; } >> heard"#;
    let dir = plugin(&crystal, "listener", manifest, &[("hook.sh", hook)]);
    crystal.ok(&["plugin", "enable", "listener"]);

    let bin = fake_claude(crystal.dir.path());
    let out = crystal
        .command(&["new", "-n", "agent", "claude"])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(out.status.success());
    let args = written(&crystal.dir.path().join("args"));
    let settings: serde_json::Value = serde_json::from_str(args.lines().nth(1).unwrap()).unwrap();
    let hook = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();

    let heard = dir.join("heard");
    let heard_of = |event: &str| {
        let heard = std::fs::read_to_string(&heard).unwrap_or_default();
        heard.lines().filter(|line| line.starts_with(event)).count()
    };
    run_hook(
        &crystal,
        "agent",
        hook,
        r#"{"hook_event_name":"UserPromptSubmit"}"#,
    );
    run_hook(
        &crystal,
        "agent",
        hook,
        r#"{"hook_event_name":"PermissionRequest"}"#,
    );
    eventually("the plugin hears it wait", || {
        heard_of("session.waiting") == 1
    });
    run_hook(&crystal, "agent", hook, r#"{"hook_event_name":"Stop"}"#);
    eventually("the plugin hears it finish", || {
        heard_of("session.done") == 1
    });

    // In the order it happened, once each, with the event as JSON.
    let heard = std::fs::read_to_string(&heard).unwrap();
    let events: Vec<&str> = heard
        .lines()
        .map(|line| line.split(' ').next().unwrap())
        .collect();
    assert_eq!(
        events,
        ["session.started", "session.waiting", "session.done"],
        "{heard}"
    );
    let waiting = heard.lines().nth(1).unwrap();
    let json = waiting
        .strip_prefix("session.waiting agent ")
        .unwrap_or_else(|| panic!("{waiting}"));
    let json: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(json["event"], "session.waiting");
    assert_eq!(json["session"]["name"], "agent");
    assert_eq!(json["session"]["activity"], "waiting");
}

#[test]
fn a_plugin_whose_hooks_keep_failing_is_paused_until_it_s_turned_on_again() {
    let crystal = Crystal::new();
    let manifest = r#"
name = "broken"
version = "1"

[[events]]
on = "*"
command = ["sh", "-c", "exit 1"]
"#;
    plugin(&crystal, "broken", manifest, &[]);
    crystal.ok(&["plugin", "enable", "broken"]);
    for n in 1..=5 {
        crystal.ok(&["new", "-d", "-n", &format!("s{n}"), "sleep", "30"]);
    }
    eventually("the plugin is paused", || {
        crystal.ok(&["plugin"]).contains("broken         paused")
    });
    let log = crystal.ok(&["plugin", "log", "broken"]);
    assert!(log.contains("exit status: 1"), "{log}");
    assert!(
        log.contains("turn it back on with `crystal plugin enable broken`"),
        "{log}"
    );

    crystal.ok(&["plugin", "enable", "broken"]);
    assert!(crystal.ok(&["plugin"]).contains("broken         on"));
}

/// A plugin with an action that writes down what it was told about where
/// it was run from.
fn where_plugin(crystal: &Crystal) -> PathBuf {
    let manifest = r#"
name = "notes"
version = "0.1.0"
description = "Notes on sessions"

[[actions]]
id = "where"
title = "Where am I"
command = ["sh", "where.sh"]
key = "N"
"#;
    let script = r#"test -x "$CRYSTAL_BIN" || exit 3
echo "$CRYSTAL_SESSION|$CRYSTAL_SESSION_ID|$CRYSTAL_PROJECT|$CRYSTAL_WORKTREE|$CRYSTAL_SOCKET" > ran
"#;
    plugin(crystal, "notes", manifest, &[("where.sh", script)])
}

#[test]
fn plugin_run_runs_an_action_with_where_it_was_run_from() {
    let crystal = Crystal::new();
    let dir = where_plugin(&crystal);
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "agent", "-c", repo_arg, "sleep", "30"]);

    let refused = crystal.fails(&["plugin", "run", "notes", "where"]);
    assert!(
        refused.contains("turn it on with `crystal plugin enable notes`"),
        "{refused}"
    );
    crystal.ok(&["plugin", "enable", "notes"]);
    crystal.ok(&["plugin", "run", "notes", "where", "--session", "agent"]);

    let ran = written(&dir.join("ran"));
    let told: Vec<&str> = ran.trim_end().split('|').collect();
    let id = listed(&crystal, "agent")["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(told[0], "agent");
    assert_eq!(told[1], id);
    assert!(told[2].ends_with("/app"), "{ran}");
    assert!(told[3].ends_with("/app"), "{ran}");
    assert_eq!(told[4], crystal.socket.to_str().unwrap());
}

#[test]
fn a_plugin_action_s_key_runs_it_from_the_sidebar() {
    let crystal = Crystal::new();
    let dir = where_plugin(&crystal);
    crystal.ok(&["plugin", "enable", "notes"]);
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "agent", "-c", repo_arg, "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("▸ agent");
    tui.type_keys("?");
    tui.shows("notes: Where am I");
    tui.type_keys("q");
    tui.hides("notes: Where am I");
    tui.type_keys("N");
    let ran = written(&dir.join("ran"));
    assert!(ran.starts_with("agent|"), "{ran}");
    tui.shows("ran notes: Where am I");
}

#[test]
fn a_plugin_pane_shows_its_program_over_the_panes_and_closes() {
    let crystal = Crystal::new();
    let manifest = r#"
name = "board"
version = "0.1.0"

[[panes]]
id = "show"
title = "The board"
command = ["sh", "show.sh"]
"#;
    let script = "echo 'the board says hi'\nread -r line\necho \"got $line\" > got\n";
    let dir = plugin(&crystal, "board", manifest, &[("show.sh", script)]);
    crystal.ok(&["plugin", "enable", "board"]);

    let mut tui = crystal.tui();
    // Down past crystal's own seven, to the pane under board.
    let open = |tui: &mut Terminal| {
        tui.type_keys("X");
        tui.shows("installed");
        tui.type_keys("jjjjjjjj\r");
        tui.shows("the board says hi");
        tui.shows("board · The board");
    };
    open(&mut tui);
    assert!(crystal.row("board-show").is_some());
    // It has the keyboard; its program ending closes it.
    tui.type_keys("hello\r");
    assert_eq!(written(&dir.join("got")), "got hello\n");
    tui.hides("the board says hi");
    eventually("its session ends with it", || {
        crystal.row("board-show").is_none()
    });

    // Ctrl+\ closes it too, and ends its session.
    open(&mut tui);
    tui.type_keys("\x1c");
    tui.hides("the board says hi");
    eventually("its session ends with it", || {
        crystal.row("board-show").is_none()
    });
}

#[test]
fn enabling_and_disabling_a_plugin_keeps_the_config_s_comments() {
    let crystal = Crystal::new();
    crystal.configure(
        "# my settings\nnotify = false # quiet\n\n[plugins]\nmemory = false # not yet\n",
    );
    crystal.ok(&["plugin", "disable", "backlog"]);
    crystal.ok(&["plugin", "enable", "memory"]);
    let config = std::fs::read_to_string(crystal.config_file()).unwrap();
    assert_eq!(
        config,
        "# my settings\nnotify = false # quiet\n\n[plugins]\nmemory = true # not yet\nbacklog = false\n"
    );
    let listed = crystal.ok(&["plugin"]);
    assert!(
        listed.contains("backlog        off    built-in"),
        "{listed}"
    );
    let refused = crystal.fails(&["backlog"]);
    assert!(
        refused
            .contains("the backlog plugin is off: turn it on with `crystal plugin enable backlog`"),
        "{refused}"
    );

    let refused = crystal.fails(&["plugin", "enable", "nope"]);
    assert!(
        refused.contains("there's no plugin called nope"),
        "{refused}"
    );
}

#[test]
fn a_leftover_top_level_memory_setting_says_where_it_went() {
    let crystal = Crystal::new();
    crystal.configure("memory = false\n");
    let refused = crystal.fails(&["plugin"]);
    assert!(refused.contains("`memory` is now a plugin"), "{refused}");
}

#[test]
fn a_plugin_installs_from_a_git_repository_and_starts_off() {
    let crystal = Crystal::new();
    let source = git_repo(crystal.dir.path(), "notes-plugin");
    let manifest = r#"name = "notes"
version = "0.2.0"
description = "Keeps notes"

[[events]]
on = "task.closed"
command = ["sh", "closed.sh"]
"#;
    std::fs::write(source.join("plugin.toml"), manifest).unwrap();
    std::fs::write(source.join("closed.sh"), "cat >> closed\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-q", "-m", "notes"]);
    let url = format!("file://{}", source.display());

    let refused = crystal.fails(&["plugin", "install", &url]);
    assert!(refused.contains("add --yes"), "{refused}");
    let said = crystal.ok(&["plugin", "install", &url, "--yes"]);
    assert!(said.contains("notes 0.2.0"), "{said}");
    assert!(said.contains("on task.closed  sh closed.sh"), "{said}");
    assert!(
        said.contains("it's off until you run `crystal plugin enable notes`"),
        "{said}"
    );
    let installed = crystal.config_home().join("crystal/plugins/notes");
    assert!(installed.join("closed.sh").exists());
    let listed = crystal.ok(&["plugin"]);
    assert!(
        listed.contains("notes          off    0.2.0     Keeps notes"),
        "{listed}"
    );

    let refused = crystal.fails(&["plugin", "install", &url, "--yes"]);
    assert!(refused.contains("notes is installed already"), "{refused}");

    crystal.ok(&["plugin", "enable", "notes"]);
    crystal.ok(&["plugin", "remove", "notes"]);
    assert!(!installed.exists());
    let config = std::fs::read_to_string(crystal.config_file()).unwrap();
    assert!(!config.contains("notes"), "{config}");

    // A directory is copied, all but its git repository.
    let source_arg = source.to_str().unwrap();
    crystal.ok(&["plugin", "install", source_arg, "--yes", "--enable"]);
    assert!(installed.join("plugin.toml").exists());
    assert!(!installed.join(".git").exists());
    assert!(crystal.ok(&["plugin"]).contains("notes          on"));
}

#[test]
fn a_new_plugin_runs_its_action_and_hears_events() {
    let crystal = Crystal::new();
    let made = crystal.ok(&["plugin", "new", "hello"]);
    assert!(made.contains("crystal plugin enable hello"), "{made}");
    crystal.ok(&["plugin", "enable", "hello"]);
    let said = crystal.ok(&["plugin", "run", "hello", "hello"]);
    assert!(said.starts_with("hello from hello: session none"), "{said}");

    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);
    let heard = crystal
        .config_home()
        .join("crystal/plugins/hello/events.jsonl");
    let line = written(&heard);
    let event: serde_json::Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
    assert_eq!(event["event"], "session.started");
    assert_eq!(event["session"]["name"], "agent");
}

/// Flows for the tests below, with the settings every test has.
const FLOWS: &str = r#"
notify = false

[plugins]
memory = false

[[profile]]
name = "builder"
agent = "claude"
model = "opus"

[[flow]]
name = "pair"

[[flow.step]]
name = "plan"
prompt = "Plan {goal}"

[[flow.step]]
name = "build"
profile = "builder"
prompt = "Build {goal} following {previous}. {feedback}"

[[flow]]
name = "gated"

[[flow.step]]
name = "plan"
prompt = "Plan {goal}"
gate = true

[[flow.step]]
name = "build"
prompt = "Build {previous}"

[[flow]]
name = "ship"

[[flow.step]]
name = "plan"
prompt = "Plan {goal}"

[[flow.step]]
name = "build"
prompt = "Build {goal} following {previous}. {feedback}"

[[flow.step]]
name = "review"
prompt = "Review {previous}"
gate = true
back_to = "build"

[[flow]]
name = "tree"

[[flow.step]]
name = "plan"
prompt = "Plan {goal}"

[[flow.step]]
name = "build"
prompt = "Build {previous}"
worktree = true

[[flow.step]]
name = "check"
prompt = "Check {previous}"
"#;

/// A stand-in for `claude -p` in a flow's steps, which keeps what it does
/// in `dir`, whatever directory a step runs in. Each run notes its
/// arguments in `runs`, a line each with the prompt's lines joined by `|`,
/// and answers at once: `answer <n>` for the n-th run, in a conversation
/// of its own, `conv-<n>`, or the one it was told to resume. A prompt with
/// `FAIL` in it fails, until the test makes `fixed`; one with `SLOW` in it
/// waits for the test to make `go` before it answers.
fn flow_claude(dir: &Path) -> PathBuf {
    let bin = dir.join("flow-bin");
    std::fs::create_dir(&bin).unwrap();
    let claude = bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
for prompt; do :; done
cd '{dir}'
echo "$*" | tr '\n' '|' >> runs
echo >> runs
run=$(wc -l < runs | tr -d ' ')
conversation="conv-$run"
previous=""
for arg; do
    if [ "$previous" = "--resume" ]; then conversation="$arg"; fi
    previous="$arg"
done
echo '{{"type":"system","subtype":"init","session_id":"'"$conversation"'","cwd":"/x","model":"m"}}'
case "$prompt" in
*SLOW*) while [ ! -e go ]; do sleep 0.05; done ;;
esac
case "$prompt" in
*FAIL*) if [ ! -e fixed ]; then
    echo '{{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"'"$conversation"'","total_cost_usd":0.01,"duration_ms":10}}'
    exit 1
fi ;;
esac
echo '{{"type":"result","subtype":"success","is_error":false,"result":"answer '"$run"'","session_id":"'"$conversation"'","total_cost_usd":0.5,"duration_ms":10}}'
"#,
        dir = dir.display()
    );
    std::fs::write(&claude, script).unwrap();
    let mut permissions = std::fs::metadata(&claude).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&claude, permissions).unwrap();
    bin
}

/// Runs `crystal flow run` with `args`, its steps finding claude on `path`.
fn run_flow(crystal: &Crystal, path: &str, args: &[&str]) -> Output {
    let mut command = crystal.command(&["flow", "run"]);
    command.args(args).env("PATH", path);
    command.output().unwrap()
}

/// Runs `crystal flow run` with `args`, which must succeed, and returns
/// what it printed.
fn flow_ok(crystal: &Crystal, path: &str, args: &[&str]) -> String {
    let out = run_flow(crystal, path, args);
    assert!(
        out.status.success(),
        "crystal flow run {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Waits for the flow run called `run` to stop running, and returns how it
/// stands.
fn flow_waits(crystal: &Crystal, run: &str) -> String {
    crystal.ok(&["flow", "wait", run, "--timeout", "10"])
}

#[test]
fn a_flow_runs_its_steps_one_after_another_and_finishes_done() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let path = path_with(&flow_claude(dir));

    let out = flow_ok(&crystal, &path, &["pair", "add", "retries", "--wait"]);
    assert_eq!(out, "pair-1\ndone\n");

    // Each step is asked its prompt, the next with the answer before it,
    // and runs with its profile.
    let runs = runs(dir, 2);
    assert!(runs[0].ends_with(" -- Plan add retries|"), "{}", runs[0]);
    assert!(
        runs[1].ends_with(" --model opus -- Build add retries following answer 1.|"),
        "{}",
        runs[1]
    );
    // Each step ran as a task of its own.
    assert_eq!(crystal.row("pair-1-plan").unwrap()[6], "task");
    assert_eq!(crystal.row("pair-1-build").unwrap()[6], "task");

    let show = crystal.ok(&["flow", "show", "pair-1"]);
    assert!(
        show.starts_with("pair-1 · pair · done · round 1 · $1.00"),
        "{show}"
    );
    assert!(show.contains("goal  add retries"), "{show}");
    let listed: serde_json::Value = serde_json::from_str(&crystal.ok(&["flow", "--json"])).unwrap();
    assert_eq!(listed[0]["name"], "pair-1");
    assert_eq!(listed[0]["state"], "done");
    assert_eq!(listed[0]["steps"][1]["answer"], "answer 2");

    // The next run of the flow gets the next number.
    let out = flow_ok(&crystal, &path, &["pair", "again", "--wait"]);
    assert_eq!(out, "pair-2\ndone\n");
}

#[test]
fn a_flow_that_isnt_in_the_config_file_says_which_are() {
    let crystal = Crystal::new();
    let none = crystal.ok(&["flow"]);
    assert!(none.contains("`crystal flow example` prints one"), "{none}");

    crystal.configure(FLOWS);
    let listed = crystal.ok(&["flow"]);
    assert!(
        listed.contains("  ship  plan → build → review\n"),
        "{listed}"
    );
    let out = run_flow(&crystal, "/nowhere", &["nope", "do it"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("there's no flow called nope; there's pair, gated, ship, tree"),
        "{err}"
    );
}

#[test]
fn a_gate_stops_the_flow_waiting_on_the_user_until_they_go_on() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let path = path_with(&flow_claude(dir));

    let out = flow_ok(&crystal, &path, &["gated", "add retries", "--wait"]);
    assert_eq!(out, "gated-1\nwaiting at plan\n");
    // Its step waits on the user, the way an agent asking something does.
    assert_eq!(status(&crystal, "gated-1-plan"), "waiting");
    let sessions: serde_json::Value = serde_json::from_str(&crystal.ok(&["ls", "--json"])).unwrap();
    assert_eq!(sessions[0]["name"], "gated-1-plan");
    assert_eq!(sessions[0]["status"], "waiting");
    let listed: serde_json::Value = serde_json::from_str(&crystal.ok(&["flow", "--json"])).unwrap();
    assert_eq!(listed[0]["state"], "waiting");
    assert_eq!(listed[0]["step"], "plan");
    assert_eq!(runs(dir, 1).len(), 1);

    crystal.ok(&["flow", "approve", "gated-1"]);
    assert_eq!(flow_waits(&crystal, "gated-1"), "done\n");
    let runs = runs(dir, 2);
    assert!(runs[1].ends_with(" -- Build answer 1|"), "{}", runs[1]);
    assert_ne!(status(&crystal, "gated-1-plan"), "waiting");

    let err = crystal.fails(&["flow", "approve", "gated-1"]);
    assert!(
        err.contains("gated-1 isn't waiting at a gate: it's done"),
        "{err}"
    );
}

#[test]
fn sending_a_flow_back_runs_its_back_to_step_again_with_the_notes() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let path = path_with(&flow_claude(dir));

    let out = flow_ok(&crystal, &path, &["ship", "add retries", "--wait"]);
    assert_eq!(out, "ship-1\nwaiting at review\n");
    runs(dir, 3);

    crystal.ok(&["flow", "back", "ship-1", "keep", "the", "old", "default"]);
    assert_eq!(flow_waits(&crystal, "ship-1"), "waiting at review\n");
    let runs = runs(dir, 5);
    // Build again, as a follow-up in its own conversation, told the notes
    // and what the review said.
    let build = &runs[3];
    assert!(
        build.contains("--resume conv-2 -- Build add retries"),
        "{build}"
    );
    assert!(
        build.contains("sent back at the review step, with these notes: keep the old default"),
        "{build}"
    );
    assert!(build.contains("What review said:|answer 3"), "{build}");
    // Then the review again, in its own conversation, of the new build.
    assert!(
        runs[4].contains("--resume conv-3 -- Review answer 4"),
        "{}",
        runs[4]
    );
    // In the same sessions, not new ones.
    assert!(crystal.row("ship-1-build-2").is_none());

    let show = crystal.ok(&["flow", "show", "ship-1"]);
    assert!(show.contains("waiting at review · round 2"), "{show}");
    crystal.ok(&["flow", "approve", "ship-1"]);
    assert_eq!(flow_waits(&crystal, "ship-1"), "done\n");
}

#[test]
fn a_failed_step_stops_the_flow_until_it_runs_again() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let path = path_with(&flow_claude(dir));

    let out = run_flow(&crystal, &path, &["pair", "FAIL", "--wait"]);
    assert!(!out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "pair-1\n");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("failed at plan"), "{err}");
    let show = crystal.ok(&["flow", "show", "pair-1"]);
    assert!(show.contains("failed at plan"), "{show}");
    assert_eq!(runs(dir, 1).len(), 1, "build never started");

    std::fs::write(dir.join("fixed"), "").unwrap();
    crystal.ok(&["flow", "retry", "pair-1"]);
    assert_eq!(flow_waits(&crystal, "pair-1"), "done\n");
    // Run again in a task that carries the failed one's conversation on.
    let runs = runs(dir, 3);
    assert!(
        runs[1].contains("--resume conv-1 -- Plan FAIL"),
        "{}",
        runs[1]
    );
    assert_eq!(status(&crystal, "pair-1-plan"), "idle");
}

#[test]
fn a_step_cut_short_by_a_restart_is_interrupted_until_it_runs_again() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let path = path_with(&flow_claude(dir));
    // A daemon of the test's own, to crash, that finds the fake claude:
    // after a restart, steps start from the daemon's environment.
    let start_daemon = || {
        let daemon = crystal
            .command(&["daemon"])
            .env("PATH", &path)
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        eventually("the daemon is listening", || crystal.socket.exists());
        daemon
    };
    let daemon = start_daemon();
    assert_eq!(
        flow_ok(&crystal, &path, &["pair", "SLOW", "down"]),
        "pair-1\n"
    );
    runs(dir, 1);
    eventually("the step is saved in its conversation", || {
        crystal.saved().contains("conv-1")
    });

    crash(daemon);
    let daemon = start_daemon();
    let listed = crystal.ok(&["flow"]);
    assert!(listed.contains("interrupted"), "{listed}");

    std::fs::write(dir.join("go"), "").unwrap();
    crystal.ok(&["flow", "retry", "pair-1"]);
    assert_eq!(flow_waits(&crystal, "pair-1"), "done\n");
    // Run again in the task that came back at rest, in its conversation.
    let runs = runs(dir, 3);
    assert!(
        runs[1].contains("--resume conv-1 -- Plan SLOW down"),
        "{}",
        runs[1]
    );
    crash(daemon);
}

#[test]
fn a_step_that_wants_a_worktree_runs_in_one_the_flow_makes_as_do_those_after() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let path = path_with(&flow_claude(dir));
    let repo = git_repo(dir, "app");

    let repo_arg = repo.to_string_lossy();
    let out = flow_ok(
        &crystal,
        &path,
        &["tree", "add retries", "-c", &repo_arg, "--wait"],
    );
    assert_eq!(out, "tree-1\ndone\n");
    // Columns: BRANCH, then DIRECTORY.
    let plan = crystal.row("tree-1-plan").unwrap();
    assert_eq!(plan[4], "main");
    for step in ["tree-1-build", "tree-1-check"] {
        let row = crystal.row(step).unwrap();
        assert_eq!(row[4], "add-retries");
        assert!(row[5].ends_with("app.worktrees/add-retries"), "{}", row[5]);
    }

    // A second run of the same goal makes a worktree of its own.
    let out = flow_ok(
        &crystal,
        &path,
        &["tree", "add retries", "-c", &repo_arg, "--wait"],
    );
    assert_eq!(out, "tree-2\ndone\n");
    assert_eq!(crystal.row("tree-2-build").unwrap()[4], "add-retries-2");
}

#[test]
fn the_sidebar_groups_a_flow_runs_steps_and_g_goes_on_past_its_gate() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let path = path_with(&flow_claude(dir));
    let out = flow_ok(&crystal, &path, &["gated", "add retries", "--wait"]);
    assert_eq!(out, "gated-1\nwaiting at plan\n");

    let mut tui = crystal.tui();
    tui.shows("◇ gated add retries");
    tui.shows("▲ plan");
    tui.shows("· build");
    tui.shows("g go on");
    tui.type_keys("g");
    tui.shows("✓ build");
    assert_eq!(flow_waits(&crystal, "gated-1"), "done\n");
}

#[test]
fn f_sends_a_flow_back_from_its_gate_with_the_notes_typed() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let path = path_with(&flow_claude(dir));
    let out = flow_ok(&crystal, &path, &["gated", "add retries", "--wait"]);
    assert_eq!(out, "gated-1\nwaiting at plan\n");

    let mut tui = crystal.tui();
    tui.shows("▲ plan");
    tui.type_keys("f");
    tui.shows("send back; what to do differently:");
    tui.type_keys("shorter\r");
    let runs = runs(dir, 2);
    assert!(
        runs[1].contains("--resume conv-1 -- Plan add retries"),
        "{}",
        runs[1]
    );
    assert!(runs[1].contains("with these notes: shorter"), "{}", runs[1]);
    tui.shows("round 2");
}

#[test]
fn with_the_flows_plugin_off_its_commands_say_so() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    crystal.ok(&["plugin", "disable", "flows"]);
    let err = crystal.fails(&["flow"]);
    assert!(err.contains("the flows plugin is off"), "{err}");
    let out = run_flow(&crystal, "/nowhere", &["pair", "add retries"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("the flows plugin is off"), "{err}");
}
