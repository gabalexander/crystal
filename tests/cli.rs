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
        // the test sees, and no test pops up a real notification or plays
        // a sound (nor one that configures its own: see `QUIET`). Memory is
        // off unless a test turns it on, so Claude's arguments stay as each
        // test expects them, and so is naming a session from its prompt, so
        // its name does. So are panes' scrollbars, so a pane's screen is as
        // wide as the pane, as the tests that count its columns expect. A
        // shell isn't a login shell, which on a Mac it would be, so it starts
        // the same on every machine. And `q` quits without asking first.
        crystal.configure(
            "notify = false\nname_from_prompt = false\nconfirm_quit = false\n\n\
             [plugins]\nmemory = false\n\n\
             [sound]\nenabled = false\n\n[mouse]\nscrollbars = false\n\n\
             [terminal]\nshell_mode = \"non_login\"\n",
        );
        crystal
    }

    /// Where the test's config lives: its `XDG_CONFIG_HOME`.
    fn config_home(&self) -> PathBuf {
        self.dir.path().join("config")
    }

    fn config_file(&self) -> PathBuf {
        self.config_home().join("crystal/config.toml")
    }

    /// The test's own Claude Code config directory, its `CLAUDE_CONFIG_DIR`:
    /// a daemon brings the skill there up to date as it starts, so it must
    /// never be the user's own.
    fn claude_config_dir(&self) -> PathBuf {
        self.dir.path().join("claude-config")
    }

    /// Writes the test's config file.
    fn configure(&self, toml: &str) {
        std::fs::create_dir_all(self.config_home().join("crystal")).unwrap();
        std::fs::write(self.config_file(), toml).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        self.command_of(Path::new(CRYSTAL), args)
    }

    /// Like [`Crystal::command`], run by the crystal at `program`.
    fn command_of(&self, program: &Path, args: &[&str]) -> Command {
        let mut command = outside_crystal(program);
        command
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .current_dir(self.dir.path())
            .env("XDG_CONFIG_HOME", self.config_home())
            .env("CLAUDE_CONFIG_DIR", self.claude_config_dir())
            .envs(PLAIN_GIT)
            .envs(QUIET);
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
        for variable in crystal_variables() {
            command.env_remove(variable);
        }
        // A notification's click brings the TUI's terminal to the front by
        // these: never the developer's own.
        for raises in ["TMUX", "TMUX_PANE", "__CFBundleIdentifier", "WINDOWID"] {
            command.env_remove(raises);
        }
        // So that a shell crystal starts is the same everywhere.
        command.env("SHELL", "/bin/sh");
        command.env("XDG_CONFIG_HOME", self.config_home());
        command.env("CLAUDE_CONFIG_DIR", self.claude_config_dir());
        // The settings view lists the agents installed in the home, and puts
        // crystal's hooks in their settings: a home of the test's own, as
        // `crystal integration` has, never the user's.
        std::fs::create_dir_all(self.home()).unwrap();
        command.env("HOME", self.home());
        command.env("CODEX_HOME", self.codex_home());
        for moved in AGENT_DIRS {
            command.env_remove(moved);
        }
        // No TUI asks GitHub whether a newer crystal is out: the releases
        // are nowhere, unless a test says where.
        command.env("CRYSTAL_RELEASES", NO_RELEASES);
        for (key, value) in PLAIN_GIT.iter().chain(&QUIET).chain(env) {
            command.env(key, value);
        }
        let child = pty.slave.spawn_command(command).unwrap();
        drop(pty.slave);

        let screen = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        let written = Arc::new(Mutex::new(Vec::new()));
        let mut output = pty.master.try_clone_reader().unwrap();
        let reader = thread::spawn({
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
            reader,
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
    /// Reads what crystal writes onto `screen` and `written`, until the
    /// terminal closes.
    reader: thread::JoinHandle<()>,
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

    /// Whether the cell at `(column, row)` of the screen is underlined.
    fn underlined(&self, column: u16, row: u16) -> bool {
        let parser = self.screen.lock().unwrap();
        parser
            .screen()
            .cell(row, column)
            .is_some_and(|cell| cell.underline())
    }

    /// Whether crystal has asked this terminal to mark pastes as pastes.
    fn marks_pastes(&self) -> bool {
        self.screen.lock().unwrap().screen().bracketed_paste()
    }

    /// Whether crystal last asked this terminal to have the wheel send the
    /// arrow keys on its alternate screen (1007), which vt100 doesn't keep.
    fn wheel_sends_arrows(&self) -> bool {
        let written = self.written.lock().unwrap();
        let written = String::from_utf8_lossy(&written);
        written.rfind("\x1b[?1007h") > written.rfind("\x1b[?1007l")
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

    /// Waits until crystal has written `bytes` to this terminal `times`
    /// times, like a sequence vt100 doesn't keep.
    fn wrote(&self, bytes: &str, times: usize) {
        let count = || {
            let written = self.written.lock().unwrap();
            let windows = written.windows(bytes.len());
            windows.filter(|window| *window == bytes.as_bytes()).count()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while count() < times {
            assert!(
                Instant::now() < deadline,
                "{bytes:?} was written {} times, not {times}",
                count()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// The rows of the screen, each as wide as the screen.
    fn rows(&self) -> Vec<String> {
        let parser = self.screen.lock().unwrap();
        let (_, width) = parser.screen().size();
        parser.screen().rows(0, width).collect()
    }

    /// How many times crystal rang this terminal's bell: each BEL that
    /// doesn't end an OSC sequence, like a request for the clipboard.
    fn bells(&self) -> usize {
        let written = self.written.lock().unwrap().clone();
        let (mut bells, mut in_osc, mut before) = (0, false, 0);
        for byte in written {
            match byte {
                b']' if before == 0x1b => in_osc = true,
                b'\\' if before == 0x1b => in_osc = false,
                0x07 if in_osc => in_osc = false,
                0x07 => bells += 1,
                _ => {}
            }
            before = byte;
        }
        bells
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

    /// Waits for crystal to exit, and for what it wrote on its way out to
    /// be read, and says whether it succeeded.
    fn exit(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                eventually("the terminal is read to its end", || {
                    self.reader.is_finished()
                });
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

/// Where a TUI looks for crystal's releases unless a test says: a port
/// nothing listens on, so it learns nothing, at once.
const NO_RELEASES: &str = "http://127.0.0.1:9/releases";

/// Keeps the machine's own git config, like signed commits or hooks, out of
/// the git that tests and crystal run.
const PLAIN_GIT: [(&str, &str); 2] = [
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
];

/// No sound from any daemon a test starts, whatever its config says; and
/// none of memory's models, which search by meaning is on for by default: a
/// cache with none of them in it, and no daemon downloading them as it
/// starts (a test that asks for them gives a cache of its own).
const QUIET: [(&str, &str); 3] = [
    ("CRYSTAL_NO_SOUND", "1"),
    ("XDG_CACHE_HOME", "/nonexistent/crystal-tests/cache"),
    ("CRYSTAL_NO_MODEL_DOWNLOAD", "1"),
];

/// The `CRYSTAL_*` variables the tests were run with: those of the crystal
/// session they may be run in, as every agent crystal starts is. Its
/// session, its daemon's socket and server, and the hooks it was given would
/// reach the commands the tests run and the hooks those run, so every one of
/// them is taken out; a test that wants one sets it itself.
fn crystal_variables() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| name.to_string_lossy().starts_with("CRYSTAL_"))
        .collect()
}

/// A command for `program`, without [`crystal_variables`]: how every test
/// runs anything.
fn outside_crystal(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for variable in crystal_variables() {
        command.env_remove(variable);
    }
    command
}

/// Runs git in `dir` the way the tests need it, failing the test if git
/// fails, and returns what it printed.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = outside_crystal("git")
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

/// The settings crystal gave Claude Code, written down one argument a
/// line in `args`: what follows `--settings`.
fn claude_settings(args: &str) -> serde_json::Value {
    let mut args = args.lines();
    args.find(|arg| *arg == "--settings").unwrap();
    serde_json::from_str(args.next().unwrap()).unwrap()
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
    // It notes the hang-up, and carries on.
    let script = "trap 'touch hung-up' HUP; while true; do sleep 1; done";
    crystal.ok(&["new", "-n", "stubborn", "sh", "-c", script]);
    let pid = crystal.pid("stubborn");

    crystal.ok(&["kill", "stubborn"]);
    eventually("it has been hung up on", || {
        crystal.dir.path().join("hung-up").exists()
    });
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
    eventually("the daemon is listening", || crystal.listening());
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
fn attach_passes_the_session_s_bell_on_to_your_terminal() {
    let crystal = Crystal::new();
    let script = r"printf ready; read line; printf '\a\a\arang'; sleep 30";
    crystal.ok(&["new", "-n", "ringer", "sh", "-c", script]);

    let mut terminal = crystal.attach(&["attach", "ringer"]);
    terminal.shows("ready");
    assert_eq!(terminal.bells(), 0);
    terminal.type_keys("\r");
    terminal.shows("rang");
    // Three at once ring it once.
    eventually("the bell is passed on", || terminal.bells() == 1);
    thread::sleep(Duration::from_millis(600));
    assert_eq!(terminal.bells(), 1);

    terminal.type_keys("\x1c");
    terminal.shows("[detached from ringer]");
}

#[test]
fn the_tui_rings_your_terminal_for_a_bell_in_a_pane_or_out_of_sight() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let rings_when = |file: &str| {
        format!("echo waiting; while [ ! -f {file} ]; do sleep 0.05; done; printf '\\a'; sleep 30")
    };
    crystal.ok(&["new", "-n", "shown", "sh", "-c", &rings_when("ring-shown")]);
    crystal.ok(&[
        "new",
        "-n",
        "builder",
        "sh",
        "-c",
        &rings_when("ring-builder"),
    ]);
    let mut tui = crystal.tui();
    tui.shows("waiting");
    assert_eq!(tui.bells(), 0);

    // The session in the pane rings: the bell is passed on, and the
    // session isn't marked, since it was seen.
    std::fs::write(dir.join("ring-shown"), "").unwrap();
    eventually("the pane's bell is passed on", || tui.bells() == 1);

    // One out of sight rings: it's marked, and rings the terminal too.
    thread::sleep(Duration::from_millis(600));
    std::fs::write(dir.join("ring-builder"), "").unwrap();
    eventually("builder is marked", || {
        sidebar_of(&tui.text()).contains("♪")
    });
    eventually("its bell is passed on", || tui.bells() == 2);
    let events = crystal.ok(&["events", "-k", "session.bell"]);
    assert!(
        events.contains("builder") && !events.contains("shown"),
        "{events}"
    );

    // Looking at it takes the mark off.
    tui.type_keys("j");
    tui.hides("♪");
}

/// A script that says it's waiting, then copies `text` with OSC 52 once
/// the file `file` is there, as Claude Code, vim or tmux would, and says
/// it's done.
fn copies_when(file: &str, text: &str) -> String {
    format!(
        "echo waiting; while [ ! -f {file} ]; do sleep 0.05; done; \
         printf '\\033]52;c;{}\\007'; echo done copying; sleep 30",
        base64(text.as_bytes())
    )
}

#[test]
fn what_a_program_in_a_pane_copies_goes_on_the_clipboard_but_not_out_of_sight() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let shown = copies_when("copy-shown", "from the pane");
    crystal.ok(&["new", "-n", "shown", "sh", "-c", &shown]);
    let hidden = copies_when("copy-hidden", "from out of sight");
    crystal.ok(&["new", "-n", "hidden", "sh", "-c", &hidden]);
    let tui = tui_over_ssh(&crystal);
    tui.shows("waiting");

    // The session in the pane copies: it goes on the clipboard, here the
    // terminal's, asked with OSC 52 since the TUI runs as over ssh.
    std::fs::write(dir.join("copy-shown"), "").unwrap();
    tui.copies("from the pane");
    tui.shows("copied 1 line");

    // One out of sight copies: nobody saw it, so it's dropped, and said.
    std::fs::write(dir.join("copy-hidden"), "").unwrap();
    tui.shows("hidden copied out of sight: not put on your clipboard");
    let events = crystal.ok(&["events", "-k", "session.copy_dropped"]);
    assert!(
        events.contains("hidden") && !events.contains("shown"),
        "{events}"
    );
    let written = String::from_utf8_lossy(&tui.written.lock().unwrap()).into_owned();
    assert!(!written.contains(&base64(b"from out of sight")));
}

#[test]
fn attach_puts_what_its_program_copies_on_the_clipboard_unless_told_not_to() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let script = copies_when("copy-now", "from attach");
    crystal.ok(&["new", "-n", "copier", "sh", "-c", &script]);
    let over_ssh = [("SSH_TTY", "/dev/ttys999")];
    let terminal = crystal.attach_with_env(&["attach", "copier"], &over_ssh);
    terminal.shows("waiting");
    std::fs::write(dir.join("copy-now"), "").unwrap();
    terminal.copies("from attach");
    drop(terminal);

    // With programs kept off the clipboard, an attach asks for nothing.
    crystal.configure(
        "notify = false\nname_from_prompt = false\n\n[plugins]\nmemory = false\n\n\
         [sound]\nenabled = false\n\n[clipboard]\nallow_programs = false\n",
    );
    let script = copies_when("copy-again", "kept off");
    crystal.ok(&["new", "-n", "kept", "sh", "-c", &script]);
    let mut terminal = crystal.attach_with_env(&["attach", "kept"], &over_ssh);
    terminal.shows("waiting");
    std::fs::write(dir.join("copy-again"), "").unwrap();
    terminal.shows("done copying");
    // What the terminal echoes of a key is drawn after the copy, which an
    // attach that passed it on would have asked for by then.
    terminal.type_keys("echoed");
    terminal.shows("echoed");
    let written = String::from_utf8_lossy(&terminal.written.lock().unwrap()).into_owned();
    assert!(!written.contains("\x1b]52;"), "{written:?}");
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
fn attach_has_the_wheel_send_arrows_only_while_the_program_is_on_the_alternate_screen() {
    let crystal = Crystal::new();
    let script = "echo at the prompt; while [ ! -f paging ]; do sleep 0.05; done; \
                  printf '\\033[?1049hin a pager'; sleep 30";
    crystal.ok(&["new", "-n", "scroller", "sh", "-c", script]);

    // Your terminal is on its alternate screen under the attach, but a
    // shell on the main one would take the arrows for its history.
    let mut terminal = crystal.attach(&["attach", "scroller"]);
    terminal.shows("at the prompt");
    terminal.wrote("\x1b[?1007l", 1);
    assert!(!terminal.wheel_sends_arrows());

    std::fs::write(crystal.dir.path().join("paging"), "").unwrap();
    terminal.shows("in a pager");
    eventually("the wheel sends arrows", || terminal.wheel_sends_arrows());

    // Detached, your terminal has it as it was before.
    terminal.type_keys("\x1c");
    terminal.shows("[detached from scroller]");
    terminal.wrote("\x1b[?1007s", 1);
    terminal.wrote("\x1b[?1007r", 1);
}

/// The test's config, with the attach taking the mouse, and a notch of
/// the wheel five lines.
const ATTACH_TAKES_THE_MOUSE: &str = "notify = false\nname_from_prompt = false\n\n\
    [plugins]\nmemory = false\n\n[sound]\nenabled = false\n\n\
    [mouse]\nattach_capture = true\nscroll_lines = 5\n";

#[test]
fn attach_taking_the_mouse_scrolls_the_history_with_the_wheel_until_you_type() {
    let crystal = Crystal::new();
    crystal.configure(ATTACH_TAKES_THE_MOUSE);
    let script = "seq 1 100; read line; echo \"got $line\"; sleep 30";
    crystal.ok(&["new", "-n", "counter", "sh", "-c", script]);
    crystal.ok(&["wait", "counter", "--output", "100"]);

    let mut terminal = crystal.attach(&["attach", "counter"]);
    terminal.shows("100");
    eventually("the mouse is taken", || terminal.sends_the_mouse());
    let top = |terminal: &Terminal| {
        terminal.rows()[0]
            .split_whitespace()
            .next()
            .map(String::from)
    };
    assert_eq!(top(&terminal).as_deref(), Some("78"));

    // A notch of the wheel up, as your terminal writes it the SGR way:
    // back into rows from before the attach.
    terminal.type_keys("\x1b[<64;10;10M");
    terminal.shows("↑ 5 lines");
    assert_eq!(top(&terminal).as_deref(), Some("73"));
    terminal.type_keys("\x1b[<64;10;10M\x1b[<64;10;10M\x1b[<65;10;10M");
    terminal.shows("↑ 10 lines");
    assert_eq!(top(&terminal).as_deref(), Some("68"));

    // Typing brings it back to live.
    terminal.type_keys("hi\r");
    terminal.shows("got hi");
    terminal.hides("↑");

    terminal.type_keys("\x1c");
    terminal.shows("[detached from counter]");
    assert!(!terminal.sends_the_mouse());
}

#[test]
fn attach_taking_the_mouse_hands_it_to_a_program_that_asked_written_its_way() {
    let crystal = Crystal::new();
    crystal.configure(ATTACH_TAKES_THE_MOUSE);
    // It asks for clicks the old way, one byte a number.
    let script = r"stty raw -echo; printf '\033[?1000hasking'; head -c 6 > clicked; \
                   printf ' clicked'; sleep 30";
    crystal.ok(&["new", "-n", "clicker", "sh", "-c", script]);

    let mut terminal = crystal.attach(&["attach", "clicker"]);
    terminal.shows("asking");
    eventually("the mouse is taken", || terminal.sends_the_mouse());
    terminal.wrote("\x1b[?1006h", 1);
    terminal.type_keys("\x1b[<0;3;2M");
    terminal.shows("clicked");
    let clicked = std::fs::read(crystal.dir.path().join("clicked")).unwrap();
    assert_eq!(clicked, b"\x1b[M #\"");
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
    // Attaching sized it first: it was 120 by 40 until then.
    let file = crystal.dir.path().join("size");
    eventually("the program hears its new size", || {
        std::fs::read_to_string(&file).is_ok_and(|size| size == "30 100\n")
    });
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
    // after a word on where that comes from, then how to work on several
    // things at once. One argument a line, and a blank line between the
    // paragraphs.
    let args = written(&crystal.dir.path().join("args"));
    let args: Vec<&str> = args.lines().collect();
    // It may run the crystal commands it's told to without asking.
    assert_eq!(args.len(), 12, "{args:?}");
    assert_eq!(args[..2], ["--allowedTools", ALLOWED]);
    assert_eq!(args[2], "--settings");
    assert_eq!(args[4], "--append-system-prompt");
    assert!(
        args[5].starts_with("You're running inside crystal"),
        "{args:?}"
    );
    assert!(args[7].contains("crystal done"), "{args:?}");
    assert!(
        args[9].starts_with("To work on several things at once"),
        "{args:?}"
    );
    assert_eq!(args[10..], ["--", "fix the login bug"]);
}

#[test]
fn the_task_is_edited_by_words_and_kept_as_a_draft_until_a_session_starts() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);

    tui.type_keys("n");
    tui.shows("What should it do?");
    tui.type_keys("fix the flaky login test");
    tui.shows("runs  claude -- 'fix the flaky login test'");
    // Ctrl+W takes the word before the cursor.
    tui.type_keys("\x17");
    tui.shows("runs  claude -- 'fix the flaky login'");
    // Alt+B goes back a word, and Alt+Backspace takes the one before that.
    tui.type_keys("\x1bb\x1b\x7f");
    tui.shows("runs  claude -- 'fix the login'");
    // Ctrl+K takes the rest of the line.
    tui.type_keys("\x0bbug");
    tui.shows("runs  claude -- 'fix the bug'");

    // Esc puts the panel away, and the next `n` opens on what was in it.
    tui.type_keys("\x1b");
    tui.hides("New session");
    tui.type_keys("n");
    tui.shows("draft left last time; esc keeps it");
    tui.shows("runs  claude -- 'fix the bug'");
    tui.type_keys("\r");
    tui.shows("▸ claude");
    let args = written(&crystal.dir.path().join("args"));
    assert!(args.ends_with("\n--\nfix the bug\n"), "{args:?}");

    // A session started from it, it's gone. The session has the keyboard:
    // Ctrl+\\ hands it back to the sidebar.
    tui.type_keys("\x1c");
    tui.hides("typing into");
    tui.type_keys("n");
    tui.shows("What should it do?");
    assert!(!tui.text().contains("draft left"), "{}", tui.text());
}

#[test]
fn a_task_pasted_whole_keeps_its_lines() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    // Asked for once the TUI has the alternate screen, which the attach
    // waits for.
    eventually("the TUI asks for pastes marked", || tui.marks_pastes());

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
    // Codex hears how to close its task in its developer instructions,
    // ahead of what it was asked.
    let args = codex_args(&crystal);
    assert_eq!(args[0], "-c");
    let model = args.iter().position(|arg| arg == "-m").unwrap();
    let told = developer_instructions(&args[1..model].join("\n"));
    assert!(told.starts_with("You're running inside crystal"), "{told}");
    assert!(told.contains("crystal done"), "{told}");
    assert_eq!(args[model..], ["-m", "gpt-test-mini", "--", "add a test"]);
}

/// The developer instructions a Codex `-c` setting gives, read the way
/// Codex reads it: as TOML.
fn developer_instructions(setting: &str) -> String {
    let setting: toml::Table = toml::from_str(setting).unwrap();
    setting["developer_instructions"]
        .as_str()
        .unwrap()
        .to_string()
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
fn capital_d_starts_a_session_like_the_selected_one() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let out = crystal
        .command(&[
            "new", "-n", "first", "-t", "fix it", "claude", "--model", "opus",
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let args = crystal.dir.path().join("args");
    written(&args);

    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("first");
    tui.type_keys("D");
    tui.shows("New session");
    tui.shows("claude --model opus");
    tui.type_keys("now the docs\r");
    eventually("the new session has started", || {
        std::fs::read_to_string(&args).is_ok_and(|args| args.contains("now the docs"))
    });
    let args = written(&args);
    let args: Vec<&str> = args.lines().collect();
    assert!(
        args.windows(2).any(|w| w == ["--model", "opus"]),
        "{args:?}"
    );
    assert!(!args.contains(&"fix it"), "{args:?}");
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
    // Named for what it was asked.
    tui.shows("▸ refund-fix");

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
    tui.shows("▸ tidy");

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
    // seen. Keys are read in turn: once the help a `?` after it opens shows,
    // the line feed has been read.
    let mut tui = crystal.tui();
    tui.shows("❯ first");
    tui.type_keys("\n?");
    tui.shows("In the sidebar");
    tui.type_keys("?");
    tui.hides("In the sidebar");
    assert!(tui.text().contains("❯ first"), "{}", tui.text());
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
fn q_asks_before_it_quits_unless_the_settings_say_not_to() {
    let crystal = Crystal::new();
    crystal.configure("notify = false\n\n[plugins]\nmemory = false\n\n[sound]\nenabled = false\n");
    crystal.ok(&["new", "-n", "stays", "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("❯ stays");
    tui.type_keys("q");
    tui.shows("quit crystal? The sessions keep running. y/n");
    // Any key but `y` stays.
    tui.type_keys("n");
    tui.hides("quit crystal?");
    tui.shows("❯ stays");
    tui.type_keys("q");
    tui.shows("quit crystal?");
    tui.type_keys("y");
    assert!(tui.exit());
    assert_eq!(crystal.row("stays").unwrap()[1], "running");
}

#[test]
fn hash_shows_the_memory_each_session_takes() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "stays", "sleep", "30"]);
    crystal.ok(&["new", "-n", "other", "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("❯ stays");
    tui.type_keys("#");
    tui.shows(" RAM · ");
    tui.shows("1 process");
    tui.shows("crystal itself");
    tui.shows("the daemon");
    tui.shows("this TUI");
    // Enter goes to the session the bar is on.
    tui.type_keys("j\r");
    tui.hides("crystal itself");
}

#[test]
fn plus_adds_a_project_and_makes_it_a_git_repository_first() {
    let crystal = Crystal::new();
    let mut tui = crystal.tui();
    tui.shows("No sessions yet");
    tui.type_keys("+");
    tui.shows("add project:");
    let dir = crystal.dir.path().join("payments");
    tui.type_keys(&format!("\x15{}\r", dir.display()));
    tui.shows("make a new git repository and add it:");
    tui.type_keys("y");
    tui.shows("payments is on the list of projects");
    assert!(dir.join(".git").exists());
    assert!(crystal.ok(&["project"]).contains("payments"));
}

#[test]
fn a_question_mark_shows_every_key_and_the_next_key_only_closes_it() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "stays", "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("? keys");
    tui.type_keys("?");
    tui.shows("In the sidebar");
    tui.shows("all needing you");
    // 80 by 24 takes three pages; space turns to the next.
    tui.shows("1/3");
    tui.type_keys(" ");
    tui.shows("In resize mode");
    tui.type_keys(" ");
    tui.shows("With the mouse");

    // q puts the keys away; it doesn't quit.
    tui.type_keys("q");
    eventually("the keys are put away", || {
        !tui.text().contains("With the mouse")
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

/// A crystal whose config is the tests' own but for `[mouse]`, which is
/// `mouse`.
fn crystal_with_mouse(mouse: &str) -> Crystal {
    let crystal = Crystal::new();
    crystal.configure(&format!(
        "notify = false\nname_from_prompt = false\n\n[plugins]\nmemory = false\n\n\
         [sound]\nenabled = false\n\n[mouse]\n{mouse}\n"
    ));
    crystal
}

#[test]
fn a_double_click_copies_a_word_and_a_triple_click_its_line() {
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
    // "beta" is at columns 36 to 39 of row 3, counting from 1, and
    // "gamma" at 41 to 45.
    tui.type_keys(&"\x1b[<0;37;3M\x1b[<0;37;3m".repeat(2));
    tui.copies("beta");
    tui.type_keys(&"\x1b[<0;43;3M\x1b[<0;43;3m".repeat(3));
    tui.copies("alpha beta gamma");
}

#[test]
fn a_drag_past_a_panes_top_scrolls_back_through_its_history_selecting() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "printer", "sh", "-c", LONG_OUTPUT]);
    written(&crystal.dir.path().join("printed"));

    let mut tui = tui_over_ssh(&crystal);
    tui.shows("line 60");
    // Down on the first letter of the pane's top row, "line 41", then up
    // onto the top bar, past the pane's header, and held there: the
    // history scrolls under it to its start, 41 rows back.
    tui.type_keys("\x1b[<0;30;3M\x1b[<32;30;1M");
    tui.shows("↑ 41 lines");
    tui.type_keys("\x1b[<0;30;1m");
    let history: Vec<String> = (1..=40).map(|i| format!("line {i}")).collect();
    tui.copies(&format!("first-line\n{}\nl", history.join("\n")));
}

#[test]
fn without_copy_on_select_a_drag_waits_in_copy_mode_for_y() {
    let crystal = crystal_with_mouse("copy_on_select = false");
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
    tui.type_keys("\x1b[<0;36;3M\x1b[<32;39;3M\x1b[<0;39;3m");
    tui.shows("words · copy mode");
    let written = || String::from_utf8_lossy(&tui.written.lock().unwrap()).to_string();
    assert!(!written().contains("\x1b]52;"), "nothing is copied yet");
    tui.type_keys("y");
    tui.copies("beta");
    // Copy mode is over, and the keyboard is back in the pane it was in:
    // the terminal echoes what's typed.
    tui.type_keys("typed");
    tui.shows("typed");
}

#[test]
fn without_mouse_capture_the_terminal_keeps_the_mouse() {
    let crystal = crystal_with_mouse("capture = false");
    crystal.ok(&["new", "-n", "stays", "sleep", "30"]);

    let mut tui = crystal.tui();
    tui.shows("❯ stays");
    assert!(!tui.sends_the_mouse());
    assert!(tui.marks_pastes(), "pastes are still the TUI's");

    // Switched on in the settings view's mouse tab, the TUI takes it
    // straight away.
    tui.type_keys(",4");
    tui.shows("○ take the mouse");
    tui.type_keys(" ");
    eventually("the TUI takes the mouse", || tui.sends_the_mouse());
    assert!(
        std::fs::read_to_string(crystal.config_file())
            .unwrap()
            .contains("capture = true")
    );
}

#[test]
fn a_notch_of_the_wheel_scrolls_as_many_lines_as_the_config_says() {
    let crystal = crystal_with_mouse("scroll_lines = 5");
    crystal.ok(&["new", "-n", "printer", "sh", "-c", LONG_OUTPUT]);
    written(&crystal.dir.path().join("printed"));

    let mut tui = crystal.tui();
    tui.shows("line 60");
    tui.type_keys(&wheel_up(50, 10));
    tui.shows("↑ 5 lines");
}

#[test]
fn a_panes_scrollbar_shows_where_it_is_and_drags_through_its_history() {
    let crystal = crystal_with_mouse("scrollbars = true");
    let script = "trap 'stty size > size' WINCH; \
                  for i in $(seq 1 60); do echo line $i; done; \
                  while :; do sleep 0.05; done";
    crystal.ok(&["new", "-n", "printer", "sh", "-c", script]);

    let mut tui = crystal.tui();
    tui.shows("line 60");
    // The scrollbar takes the pane's last column: its session has 50.
    eventually("the session is a column narrower", || {
        std::fs::read_to_string(crystal.dir.path().join("size")).is_ok_and(|size| size == "21 50\n")
    });
    // 40 rows of history behind 21: a thumb of 7 rows, at the bottom of the
    // track, rows 3 to 23, while the pane is live.
    let column = |text: &str| -> String {
        text.lines()
            .map(|line| line.chars().nth(79).unwrap_or(' '))
            .collect()
    };
    eventually("the thumb is drawn", || {
        column(&tui.text()).contains("▐▐▐▐▐▐▐")
    });
    // Taken by its middle and dragged up past the top, it shows the
    // start of the history.
    tui.type_keys("\x1b[<0;80;20M\x1b[<32;80;1M\x1b[<0;80;1m");
    tui.shows("↑ 40 lines");
    tui.shows("line 1 ");
    // A click near the bottom of the track jumps back there.
    tui.type_keys(&click(79, 22));
    tui.hides("↑ 40 lines");
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
    let mut child = outside_crystal("sh")
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

    // crystal added its hooks and its notes ahead of the arguments it was
    // given.
    let args = written(&crystal.dir.path().join("args"));
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(args[2], "--settings");
    assert_eq!(args[4], "--append-system-prompt");
    assert_eq!(args.last(), Some(&"--resume"));
    let settings: serde_json::Value = serde_json::from_str(args[3]).unwrap();
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
fn claude_code_s_rename_names_the_session_and_a_name_given_in_crystal_goes_back() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = crystal
        .command(&["new", "claude"])
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let args = written(&crystal.dir.path().join("args"));
    let args: Vec<&str> = args.lines().collect();
    let settings: serde_json::Value = serde_json::from_str(args[3]).unwrap();
    let hook = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_string();
    let sessions: serde_json::Value = serde_json::from_str(&crystal.ok(&["ls", "--json"])).unwrap();
    let id = sessions[0]["id"].as_str().unwrap().to_string();
    let env = [
        ("CRYSTAL_SESSION", "claude"),
        ("CRYSTAL_SESSION_ID", id.as_str()),
    ];

    // Where Claude Code keeps the conversation, and its name beside it.
    let projects = crystal.dir.path().join("projects");
    let transcript = projects.join("talk-1.jsonl");
    let named = projects.join("talk-1").join("custom-title.json");
    let event = |name: &str, more: &str| {
        format!(
            r#"{{"hook_event_name":"{name}","session_id":"talk-1","transcript_path":"{}"{more}}}"#,
            transcript.display()
        )
    };
    let says = |event: &str| hook_says(&crystal, &env, &hook, event);
    assert_eq!(says(&event("SessionStart", "")), "");

    // `/rename` in Claude Code names the session.
    std::fs::create_dir_all(named.parent().unwrap()).unwrap();
    std::fs::write(&named, r#"{"customTitle":"Fix refund rounding"}"#).unwrap();
    eventually("the session follows the rename", || {
        crystal.row("fix-refund-rounding").is_some()
    });

    // A name given in crystal goes to Claude Code with the next prompt, once.
    crystal.ok(&["rename", "fix-refund-rounding", "refunds"]);
    let prompt = event("UserPromptSubmit", r#","prompt":"and the docs""#);
    let answer: serde_json::Value = serde_json::from_str(&says(&prompt)).unwrap();
    assert_eq!(answer["hookSpecificOutput"]["sessionTitle"], "refunds");
    assert_eq!(says(&prompt), "");

    // A name the user gave stays through a rename in Claude Code.
    std::fs::write(&named, r#"{"customTitle":"Something else"}"#).unwrap();
    assert_eq!(says(&event("Stop", "")), "");
    assert!(crystal.row("refunds").is_some());
    assert!(crystal.row("something-else").is_none());
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
    /// A Codex home of the test's own, its `CODEX_HOME`: `crystal
    /// integration` must never reach the user's own.
    fn codex_home(&self) -> PathBuf {
        self.dir.path().join("codex-home")
    }

    /// A home directory of the test's own, where the agents keep their
    /// settings when no variable moves them: `crystal integration` must
    /// never reach the user's own.
    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    /// Runs `crystal integration` with `args`, in the test's own home and
    /// Claude Code and Codex directories, and gives back what it printed,
    /// or why it failed.
    fn integration(&self, args: &[&str]) -> Result<String, String> {
        let mut all = vec!["integration"];
        all.extend(args);
        let mut command = self.command(&all);
        command
            .env("CODEX_HOME", self.codex_home())
            .env("HOME", self.home());
        for moved in AGENT_DIRS {
            command.env_remove(moved);
        }
        let out = command.output().unwrap();
        let said = |bytes: Vec<u8>| String::from_utf8(bytes).unwrap();
        if out.status.success() {
            Ok(said(out.stdout))
        } else {
            Err(said(out.stderr))
        }
    }
}

/// The variables that move agents' settings directories elsewhere than
/// the home directory.
const AGENT_DIRS: &[&str] = &[
    "CURSOR_CONFIG_DIR",
    "QODER_CONFIG_DIR",
    "QWEN_HOME",
    "COPILOT_HOME",
    "KIMI_CODE_HOME",
    "GROK_CONFIG_DIR",
    "GROK_HOME",
    "ANTIGRAVITY_CLI_CONFIG_DIR",
    "HERMES_HOME",
    "PI_CODING_AGENT_DIR",
];

/// The commands of the hooks on `event` in `settings`.
fn hook_commands(settings: &serde_json::Value, event: &str) -> Vec<String> {
    let groups = settings["hooks"][event]
        .as_array()
        .cloned()
        .unwrap_or_default();
    groups
        .iter()
        .flat_map(|group| group["hooks"].as_array().cloned().unwrap_or_default())
        .filter_map(|hook| hook["command"].as_str().map(String::from))
        .collect()
}

#[test]
fn integration_puts_crystal_s_hooks_beside_the_user_s_and_takes_them_out_again() {
    let crystal = Crystal::new();
    let refused = crystal.integration(&["install", "claude"]).unwrap_err();
    assert!(refused.contains("install Claude Code first"), "{refused}");

    std::fs::create_dir_all(crystal.claude_config_dir()).unwrap();
    let settings_file = crystal.claude_config_dir().join("settings.json");
    let users = serde_json::json!({
        "model": "opus",
        "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]}
    });
    std::fs::write(&settings_file, users.to_string()).unwrap();
    let status = crystal.integration(&["status", "claude"]).unwrap();
    assert!(status.starts_with("claude  not installed  "), "{status}");

    let said = crystal.integration(&["install", "claude"]).unwrap();
    assert!(said.contains("added crystal's hooks to"), "{said}");
    let read = || -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(&settings_file).unwrap()).unwrap()
    };
    let installed = read();
    assert_eq!(installed["model"], "opus");
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "SubagentStart",
        "SubagentStop",
    ] {
        let commands = hook_commands(&installed, event);
        assert_eq!(commands.len(), 1, "{event}: {commands:?}");
        assert!(commands[0].contains(CRYSTAL), "{commands:?}");
        assert!(commands[0].ends_with(" hook claude --installed"));
    }
    let stop = hook_commands(&installed, "Stop");
    assert_eq!(stop[0], "say done", "the user's own hook comes first");
    assert_eq!(stop.len(), 2);

    let again = crystal.integration(&["install", "claude"]).unwrap();
    assert!(again.contains("already"), "{again}");
    assert_eq!(read(), installed);
    let status = crystal.integration(&["status"]).unwrap();
    assert!(status.contains("claude  installed  "), "{status}");
    assert!(status.contains("codex  not installed  "), "{status}");

    let said = crystal.integration(&["uninstall", "claude"]).unwrap();
    assert!(said.contains("took crystal's hooks out"), "{said}");
    assert_eq!(read(), users);
}

#[test]
fn integration_gives_codex_crystal_s_hooks_and_turns_them_on() {
    let crystal = Crystal::new();
    let refused = crystal.integration(&["install", "codex"]).unwrap_err();
    assert!(refused.contains("install Codex first"), "{refused}");
    // With neither agent there, there's nothing to install for.
    assert!(crystal.integration(&["install"]).is_err());

    std::fs::create_dir_all(crystal.codex_home()).unwrap();
    let config = crystal.codex_home().join("config.toml");
    std::fs::write(&config, "# mine\nmodel = \"gpt-5\"\n").unwrap();
    // Without an agent named, each that's installed: only Codex here.
    let said = crystal.integration(&["install"]).unwrap();
    assert!(said.contains("codex: added crystal's hooks to"), "{said}");
    assert!(said.contains("turned hooks on"), "{said}");
    assert!(!said.contains("claude"), "{said}");
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "# mine\nmodel = \"gpt-5\"\n\n[features]\nhooks = true\n"
    );
    let hooks: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(crystal.codex_home().join("hooks.json")).unwrap(),
    )
    .unwrap();
    for event in ["SessionStart", "UserPromptSubmit", "Stop", "Interrupt"] {
        let commands = hook_commands(&hooks, event);
        assert_eq!(commands.len(), 1, "{event}");
        assert!(commands[0].ends_with(" hook codex --installed"));
    }
    let status = crystal.integration(&["status", "codex"]).unwrap();
    assert!(status.starts_with("codex  installed  "), "{status}");
}

/// A stand-in for a Claude Code typed into a shell, its hooks installed:
/// it writes down its arguments, then runs the installed hook as Claude
/// Code would, for its conversation `conv-1` starting and a subagent, and
/// waits until there's a `quit` file. Returns the directory to put on the
/// PATH.
fn fake_typed_claude(dir: &Path) -> PathBuf {
    let bin = dir.join("typed-bin");
    std::fs::create_dir(&bin).unwrap();
    let body = format!(
        r#"
hook() {{ printf '%s' "$1" | '{CRYSTAL}' hook claude --installed; }}
printf '%s\n' "$@" > claude-args.new && mv claude-args.new claude-args
printf '{{}}\n' > conv-1.jsonl
hook '{{"hook_event_name":"SessionStart","source":"startup","session_id":"conv-1","transcript_path":"'"$PWD"'/conv-1.jsonl"}}'
hook '{{"hook_event_name":"SubagentStart","session_id":"conv-1","agent_id":"a1","agent_type":"Explore"}}'
while [ ! -e quit ]; do sleep 0.05; done
"#
    );
    script(&bin.join("claude"), &body);
    bin
}

#[test]
fn a_claude_typed_into_a_shell_reports_and_is_typed_back_in_after_a_restart() {
    let crystal = Crystal::new();
    let bin = fake_typed_claude(crystal.dir.path());
    let path = path_of(&[&bin]);
    let args = crystal.dir.path().join("claude-args");
    let daemon = crystal.start_daemon();
    let out = crystal
        .command(&["new", "-d", "-n", "box", "sh"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    crystal.ok(&["send", "box", "claude"]);
    assert_eq!(written(&args), "\n", "started with no arguments");
    eventually("its resume command is saved", || {
        crystal
            .saved()
            .contains(r#""conversation":null,"task":null,"goal":null,"resume":["claude","--resume","conv-1"]"#)
    });
    // Its subagent is counted, and told of.
    assert_eq!(crystal.listed("box")["subagents"], 1);
    let told = events(&crystal, &["-n", "box", "-k", "subagent.*"]);
    assert_eq!(names(&told), ["subagent.started"]);
    assert_eq!(told[0]["subagent"]["agent_type"], "Explore");

    crash(daemon);
    std::fs::remove_file(&args).unwrap();
    let out = crystal
        .command(&["new", "-d", "-n", "other", "sleep", "300"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    // The shell comes back, and has Claude typed into it, in its
    // conversation.
    assert_eq!(written(&args), "--resume\nconv-1\n");
    assert_eq!(crystal.row("box").unwrap()[7], "sh");
    eventually("Claude is in front again", || {
        let listed = crystal.listed("box");
        listed["front"]["program"] == "claude" && listed["subagents"] == 1
    });

    // Once it quits, the shell is back, and stays the shell after a restart.
    crystal.stage("quit");
    eventually("the shell is back with nothing to resume", || {
        let listed = crystal.listed("box");
        listed["front"]["kind"] == "shell"
            && listed["subagents"] == 0
            && crystal.saved().contains(r#""command":["sh"],"cwd""#)
            && !crystal.saved().contains("conv-1")
    });
}

#[test]
fn the_installed_hooks_stay_quiet_for_a_claude_crystal_hooked_itself() {
    let crystal = Crystal::new();
    // The Claude crystal starts is told so.
    let bin = crystal.dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join("claude"),
        "printf '%s\\n' \"$CRYSTAL_AGENT_HOOKS\" > hooked.new && mv hooked.new hooked\nsleep 30\n",
    );
    let out = crystal
        .command(&["new", "-d", "-n", "agent", "claude"])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(written(&crystal.dir.path().join("hooked")), "claude\n");

    crystal.ok(&["new", "-d", "-n", "box", "sleep", "300"]);
    let installed = format!("'{CRYSTAL}' hook claude --installed");
    let prompt = r#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#;
    let hooked = [
        ("CRYSTAL_SESSION", "box"),
        ("CRYSTAL_AGENT_HOOKS", "claude"),
    ];
    run_hook_with(&crystal, &hooked, &installed, prompt);
    assert_eq!(crystal.row("box").unwrap()[1], "running");
    // A Claude typed into the shell isn't hooked: its installed hook
    // reports.
    run_hook(&crystal, "box", &installed, prompt);
    assert_eq!(crystal.row("box").unwrap()[1], "working");
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

#[test]
fn agent_explain_shows_the_rule_that_read_a_session_s_screen() {
    let crystal = Crystal::new();
    crystal.new_pretend_agent("agent", "printf '\\033]0;⠋ Thinking\\007> '; sleep 30");
    eventually("the agent is working", || {
        crystal.row("agent").unwrap()[1] == "working"
    });
    let out = crystal.ok(&["agent", "explain", "agent"]);
    assert!(
        out.starts_with("agent: Aider (aider) is in front\n"),
        "{out}"
    );
    assert!(
        out.contains("\ncrystal has its screen as working\n"),
        "{out}"
    );
    assert!(
        out.contains("\nAny other agent (default) rules read the screen now as working, by rule title_spinner\nrules: crystal's own\n"),
        "{out}"
    );
    let decided = out.lines().find(|line| line.starts_with('→')).unwrap();
    assert!(decided.contains("title_spinner"), "{decided}");

    let json = crystal.ok(&["agent", "explain", "agent", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json["rules"]["decided_by"], "title_spinner");
    assert_eq!(json["watch"], "working");
    // Another agent's rules, on the same screen.
    let codex = crystal.ok(&["agent", "explain", "agent", "--agent", "codex"]);
    assert!(
        codex.contains("\nCodex (codex) rules read the screen now as "),
        "{codex}"
    );
    let err = crystal.fails(&["agent", "explain", "nobody"]);
    assert!(err.contains("nobody"), "{err}");
}

#[test]
fn agent_explain_tries_an_agent_s_rules_on_a_saved_screen() {
    let crystal = Crystal::new();
    let screen = crystal.dir.path().join("screen.txt");
    std::fs::write(
        &screen,
        "  Would you like to run the following command?\n\n› 1. Yes, proceed (y)\n",
    )
    .unwrap();
    let file = screen.to_str().unwrap();
    let out = crystal.ok(&["agent", "explain", "--file", file, "--agent", "codex"]);
    assert!(
        out.starts_with(
            "Codex (codex) rules read the screen now as waiting, by rule approval_question\n"
        ),
        "{out}"
    );
    let titled = crystal.ok(&[
        "agent",
        "explain",
        "--file",
        file,
        "--agent",
        "codex",
        "--title",
        "Action Required",
    ]);
    assert!(titled.contains("by rule osc_title_blocked"), "{titled}");
    let err = crystal.fails(&["agent", "explain", "--file", file, "--agent", "nobody"]);
    assert!(err.contains("crystal has no rules for nobody"), "{err}");
    // The file it read is the one `rules` prints, under any of its names.
    let rules = crystal.ok(&["agent", "rules", "cursor-agent"]);
    assert!(rules.contains("\nid = \"cursor\"\n"), "{rules}");
    assert!(
        crystal
            .fails(&["agent", "rules", "nobody"])
            .contains("no rules for nobody")
    );
}

#[test]
fn a_rules_file_of_the_user_s_changes_how_an_agent_is_read() {
    let crystal = Crystal::new();
    let agents = crystal.config_home().join("crystal/agents");
    std::fs::create_dir_all(&agents).unwrap();
    // Aider has no rules of its own: these say its hum is work.
    let hum = "id = \"aider\"\nname = \"Aider\"\n\n[[rules]]\nid = \"hum\"\nlooks = \"working\"\ncontains = [\"hmm\"]\n";
    std::fs::write(agents.join("aider.toml"), hum).unwrap();
    crystal.new_pretend_agent("agent", "printf 'hmm'; sleep 30");
    eventually("the agent is working", || {
        crystal.row("agent").unwrap()[1] == "working"
    });
    let list = crystal.ok(&["agent", "list"]);
    assert!(list.starts_with("AGENT "), "{list}");
    let aider = list
        .lines()
        .find(|line| line.starts_with("aider "))
        .unwrap();
    assert!(aider.contains("agents/aider.toml"), "{aider}");
    let claude = list
        .lines()
        .find(|line| line.starts_with("claude "))
        .unwrap();
    assert!(claude.ends_with("built in"), "{claude}");

    // A broken file says so, and crystal's own rules stand in for it.
    std::fs::write(agents.join("codex.toml"), "id = \"codex\"\n[[rules]\n").unwrap();
    let list = crystal.ok(&["agent", "list"]);
    let codex = list
        .lines()
        .find(|line| line.starts_with("codex "))
        .unwrap();
    assert!(codex.contains(" bundled "), "{codex}");
    assert!(list.contains("\n! couldn't use "), "{list}");
    let json = crystal.ok(&["agent", "list", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    let codex = json["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["id"] == "codex")
        .unwrap();
    assert!(codex["problem"].as_str().unwrap().contains("codex.toml"));
}

#[test]
fn integration_puts_hooks_in_other_agents_settings_and_takes_them_out() {
    let crystal = Crystal::new();
    let cursor = crystal.dir.path().join("cursor-config");
    std::fs::create_dir_all(&cursor).unwrap();
    let run = |args: &[&str]| {
        let out = crystal
            .command(args)
            .env("CURSOR_CONFIG_DIR", &cursor)
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "crystal {args:?}: {err}");
        String::from_utf8(out.stdout).unwrap()
    };
    let hooks_line = |list: &str| {
        list.lines()
            .find(|line| line.starts_with("cursor "))
            .unwrap()
            .to_string()
    };
    let out = run(&["integration", "install", "cursor"]);
    assert!(out.contains("cursor: added crystal's hooks to"), "{out}");
    let hooks: serde_json::Value =
        serde_json::from_str(&written(&cursor.join("hooks.json"))).unwrap();
    let command = hooks["hooks"]["stop"][0]["command"].as_str().unwrap();
    assert!(command.contains(CRYSTAL), "{command}");
    assert!(command.ends_with(" hook cursor || true"), "{command}");
    let listed = hooks_line(&run(&["agent", "list"]));
    assert!(
        listed.ends_with(" installed") && !listed.contains("not installed"),
        "{listed}"
    );
    let status = run(&["integration", "status", "cursor"]);
    assert!(status.starts_with("cursor  installed"), "{status}");

    let out = run(&["integration", "uninstall", "cursor"]);
    assert!(out.contains("took crystal's hooks out"), "{out}");
    let listed = hooks_line(&run(&["agent", "list"]));
    assert!(listed.ends_with("not installed"), "{listed}");

    let gemini = crystal.fails(&["integration", "install", "gemini"]);
    assert!(gemini.contains("gemini"), "{gemini}");
}

#[test]
fn integration_gives_the_agents_that_take_plugins_or_toml_theirs() {
    let crystal = Crystal::new();
    let home = crystal.home();
    let kimi = home.join(".kimi-code");
    let pi = home.join(".pi/agent");
    let hermes = home.join(".hermes");
    for dir in [&kimi, &pi, &hermes] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let kimi_config = "# mine\ndefault_model = \"k2\"\n";
    std::fs::write(kimi.join("config.toml"), kimi_config).unwrap();
    let hermes_config = "model: x\n";
    std::fs::write(hermes.join("config.yaml"), hermes_config).unwrap();

    // Without an agent named, each that's installed: these three here.
    let said = crystal.integration(&["install"]).unwrap();
    assert!(said.contains("kimi: added crystal's hooks to"), "{said}");
    assert!(said.contains("pi: added crystal's plugin to"), "{said}");
    assert!(said.contains("hermes: added crystal's plugin to"), "{said}");
    assert!(!said.contains("letta"), "{said}");

    let config = std::fs::read_to_string(kimi.join("config.toml")).unwrap();
    assert!(config.starts_with(kimi_config), "{config}");
    assert!(config.contains("[[hooks]]\nevent = \"Stop\"\n"), "{config}");
    assert!(
        config.contains(&format!("{CRYSTAL} hook kimi --event Stop || true")),
        "{config}"
    );
    let extension = std::fs::read_to_string(pi.join("extensions/crystal.ts")).unwrap();
    assert!(
        extension.contains(&format!("const CRYSTAL = \"{CRYSTAL}\";")),
        "{extension}"
    );
    let enabled = std::fs::read_to_string(hermes.join("config.yaml")).unwrap();
    assert_eq!(enabled, "model: x\nplugins:\n  enabled:\n    - crystal\n");
    assert!(hermes.join("plugins/crystal/__init__.py").is_file());

    let status = crystal.integration(&["status"]).unwrap();
    for (agent, standing) in [
        ("kimi", "installed"),
        ("pi", "installed"),
        ("hermes", "installed"),
        ("letta", "not installed"),
        ("opencode", "not installed"),
    ] {
        assert!(
            status.contains(&format!("\n{agent}  {standing}  ")),
            "{status}"
        );
    }
    let again = crystal.integration(&["install", "pi"]).unwrap();
    assert!(again.contains("already"), "{again}");

    for agent in ["kimi", "pi", "hermes"] {
        let said = crystal.integration(&["uninstall", agent]).unwrap();
        assert!(said.contains("took crystal's"), "{said}");
    }
    assert_eq!(
        std::fs::read_to_string(kimi.join("config.toml")).unwrap(),
        kimi_config
    );
    assert!(!pi.join("extensions/crystal.ts").exists());
    assert_eq!(
        std::fs::read_to_string(hermes.join("config.yaml")).unwrap(),
        hermes_config
    );
    assert!(!hermes.join("plugins/crystal").exists());
}

/// A stand-in for Kimi Code, its hooks installed: it writes down its
/// arguments, then, unless it was started in a session of its own, runs
/// crystal's hook as Kimi would as it starts and as it's sent a prompt, in
/// its session `k-1`, and waits.
fn fake_kimi(dir: &Path) -> PathBuf {
    let bin = dir.join("kimi-bin");
    std::fs::create_dir(&bin).unwrap();
    let body = format!(
        r#"
printf '%s\n' "$@" > kimi-args.new && mv kimi-args.new kimi-args
if [ "$1" != --session ]; then
    printf '{{"session_id":"k-1"}}' | '{CRYSTAL}' hook kimi --event SessionStart
    printf '{{"session_id":"k-1","prompt":"fix it"}}' | '{CRYSTAL}' hook kimi --event UserPromptSubmit
fi
sleep 30
"#
    );
    script(&bin.join("kimi"), &body);
    bin
}

#[test]
fn an_agent_comes_back_in_the_conversation_its_hooks_named() {
    let crystal = Crystal::new();
    let bin = fake_kimi(crystal.dir.path());
    let path = path_of(&[&bin]);
    let args = crystal.dir.path().join("kimi-args");
    let daemon = crystal.start_daemon();
    let out = crystal
        .command(&["new", "-d", "-n", "kimi", "kimi", "--yolo"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(written(&args), "--yolo\n");
    eventually("its conversation, prompted, is saved", || {
        crystal
            .saved()
            .contains(r#""conversation":{"id":"k-1","transcript":null,"prompted":true}"#)
    });

    crash(daemon);
    std::fs::remove_file(&args).unwrap();
    let out = crystal
        .command(&["new", "-d", "-n", "other", "sleep", "300"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    // Kimi starts again in its session, with the options it was given.
    assert_eq!(written(&args), "--session\nk-1\n--yolo\n");
}

#[test]
fn an_agent_typed_into_a_shell_is_resumed_once_it_has_had_a_prompt() {
    let crystal = Crystal::new();
    let bin = crystal.dir.path().join("droid-bin");
    std::fs::create_dir(&bin).unwrap();
    let body = format!(
        r#"
hook() {{ printf '%s' "$1" | '{CRYSTAL}' hook droid; }}
hook '{{"hook_event_name":"SessionStart","session_id":"d-1"}}'
touch named
while [ ! -e prompt ]; do sleep 0.05; done
hook '{{"hook_event_name":"UserPromptSubmit","session_id":"d-1","prompt":"go"}}'
sleep 30
"#
    );
    script(&bin.join("droid"), &body);
    let out = crystal
        .command(&["new", "-d", "-n", "box", "sh"])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(out.status.success());
    crystal.ok(&["send", "box", "droid"]);
    eventually("Droid is in front", || {
        crystal.listed("box")["front"]["program"] == "droid"
    });
    // Named but never sent a prompt, its session has nothing to pick up:
    // not in the sessions written down once another has started since.
    eventually("Droid has named its conversation", || {
        crystal.dir.path().join("named").exists()
    });
    crystal.ok(&["new", "-n", "later", "sleep", "30"]);
    let mut written = serde_json::Value::Null;
    eventually("the sessions are written down since", || {
        written = serde_json::from_str(&crystal.saved()).unwrap_or_default();
        written.to_string().contains(r#""name":"later""#)
    });
    let sessions = written.as_array().unwrap();
    let saved = sessions.iter().find(|session| session["name"] == "box");
    assert!(saved.unwrap()["resume"].is_null(), "{written}");
    crystal.stage("prompt");
    eventually("its resume command is saved", || {
        crystal
            .saved()
            .contains(r#""resume":["droid","--resume","d-1"]"#)
    });
}

#[test]
fn a_hook_from_another_agent_than_the_one_in_front_is_left_out() {
    let crystal = Crystal::new();
    crystal.new_pretend_agent("agent", "printf '> '; sleep 30");
    eventually("the agent is in front", || {
        crystal
            .ok(&["agent", "explain", "agent"])
            .starts_with("agent: Aider (aider) is in front")
    });
    let prompt = r#"{"hook_event_name":"UserPromptSubmit","session_id":"s1"}"#;
    // A Codex the agent started, with hooks in its own settings.
    run_hook(&crystal, "agent", &format!("{CRYSTAL} hook codex"), prompt);
    assert_eq!(crystal.row("agent").unwrap()[1], "running");
    // The agent's own hooks count.
    run_hook(&crystal, "agent", &format!("{CRYSTAL} hook aider"), prompt);
    assert_eq!(crystal.row("agent").unwrap()[1], "working");
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
fn a_new_branch_starts_from_origins_main_unless_given_a_base() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "app");
    local_origin(dir, &repo, "https://example.com/app.git");
    let pushed = git(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "not pushed"],
    );
    let mine = git(&repo, &["rev-parse", "HEAD"]);
    let repo_arg = repo.to_str().unwrap();

    crystal.ok(&["new", "-d", "-c", repo_arg, "-w", "fresh", "sleep", "30"]);
    let fresh = dir.join("app.worktrees/fresh");
    assert_eq!(git(&fresh, &["rev-parse", "HEAD"]), pushed);

    crystal.ok(&[
        "new", "-d", "-c", repo_arg, "-w", "mine", "--base", "HEAD", "sleep", "30",
    ]);
    let here = dir.join("app.worktrees/mine");
    assert_eq!(git(&here, &["rev-parse", "HEAD"]), mine);

    let err = crystal.fails(&[
        "new", "-d", "-c", repo_arg, "-w", "lost", "--base", "nope", "sleep", "30",
    ]);
    assert!(
        err.contains("there's no branch, tag or commit called nope"),
        "{err}"
    );
    assert!(!dir.join("app.worktrees/lost").exists());
    let err = crystal.fails(&["new", "-d", "-c", repo_arg, "--base", "HEAD", "true"]);
    assert!(err.contains("--worktree"), "{err}");
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
fn worktree_rm_keeps_work_that_isnt_committed_unless_forced() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "x", "-c", repo_arg, "-w", "fix", "true"]);
    let worktree = crystal.dir.path().join("app.worktrees/fix");
    std::fs::write(worktree.join("notes.txt"), "half done\n").unwrap();

    let err = crystal.fails(&["worktree", "rm", "app.worktrees/fix"]);
    assert!(err.contains("untracked"), "{err}");
    assert!(worktree.join("notes.txt").exists());

    // Unless it's forced, and the work goes with it.
    crystal.ok(&["worktree", "rm", "--force", "app.worktrees/fix"]);
    assert!(!worktree.exists());
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
fn w_makes_the_new_worktree_on_a_branch_with_a_made_up_name() {
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
    let branch = panel_branch(&tui);
    assert!(branch.split_once('-').is_some(), "{branch}");
    tui.shows(&format!("New session · app ⎇ {branch}"));
    // The task doesn't rename it.
    tui.type_keys("Fix the flaky test!");
    tui.shows("Fix the flaky test!");
    assert_eq!(panel_branch(&tui), branch);
    tui.type_keys("\r");
    tui.shows("typing into");
    tui.shows(&format!("⎇ {branch}"));

    // Claude Code started in the new worktree, which writes down its
    // arguments where it runs.
    let worktree = crystal.dir.path().join("app.worktrees").join(&branch);
    let args = written(&worktree.join("args"));
    assert_eq!(args.lines().last(), Some("Fix the flaky test!"));
}

#[test]
fn a_session_started_while_a_list_of_sessions_was_on_its_way_keeps_the_keyboard() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);

    // A slow `git worktree add` keeps the TUI busy long enough for a list
    // of sessions asked for before the new one to arrive after it.
    let bin = fake_claude(crystal.dir.path());
    slow_worktree_add(&bin);
    let path = path_of(&[&bin]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("▸ planner");
    tui.type_keys("w");
    tui.shows("New session · app");
    tui.type_keys("Plan it\r");
    tui.shows("typing into");
    thread::sleep(Duration::from_millis(500));
    tui.shows("typing into");
}

/// Puts a `git` in `bin` that takes a second and a half over `worktree
/// add`, then runs the real one.
fn slow_worktree_add(bin: &Path) {
    let found = outside_crystal("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let git = String::from_utf8(found.stdout).unwrap();
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in *\"worktree add\"*) sleep 1.5 ;; esac\nexec {} \"$@\"\n",
        git.trim()
    );
    let slow = bin.join("git");
    std::fs::write(&slow, script).unwrap();
    let mut permissions = std::fs::metadata(&slow).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&slow, permissions).unwrap();
}

#[test]
fn a_new_worktrees_made_up_name_can_be_typed_over() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);

    let path = path_of(&[]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("▸ planner");
    tui.type_keys("w");
    panel_branch(&tui);
    // Shift+Tab goes round from the first row to the branch's, and Ctrl+U
    // rubs the name out, so Enter asks for one.
    tui.type_keys("\x1b[Z\x15\r");
    tui.shows("name the new worktree's branch");
    tui.type_keys("spike\r");
    let worktree = crystal.dir.path().join("app.worktrees/spike");
    eventually("the worktree is made", || worktree.is_dir());
}

/// The branch the new-session panel shows for a new worktree, once it has
/// drawn its title and the rows below it, down to the command it runs.
fn panel_branch(tui: &Terminal) -> String {
    let mut branch = String::new();
    eventually("the panel shows the new worktree's branch", || {
        let text = tui.text();
        let row = text
            .lines()
            .find_map(|line| line.split_once("branch       "))
            .map_or("", |(_, after)| after);
        branch = row
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        !branch.is_empty() && text.contains(&format!("⎇ {branch}")) && text.contains("runs  ")
    });
    branch
}

impl Crystal {
    /// Starts the daemon in a process of this test's own, so the test can
    /// kill it the way a crash would.
    fn start_daemon(&self) -> std::process::Child {
        self.start_daemon_with(&[])
    }

    /// Like [`Crystal::start_daemon`], with `env` added to its environment,
    /// and waits until it's listening: a command run before that would start
    /// a daemon of its own, without `env`.
    fn start_daemon_with(&self, env: &[(&str, &str)]) -> std::process::Child {
        let daemon = self
            .command(&["daemon"])
            .envs(env.iter().copied())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        eventually("the daemon is listening", || self.listening());
        daemon
    }

    /// Whether a daemon is listening on the test's socket: one killed
    /// outright leaves the socket behind, with nothing listening on it.
    fn listening(&self) -> bool {
        std::os::unix::net::UnixStream::connect(&self.socket).is_ok()
    }

    /// The sessions the daemon has written down in its database, as a JSON
    /// list, or nothing when there are none.
    fn saved(&self) -> String {
        let list = "SELECT json_group_array(json_object('name', name, 'command', json(command), \
                    'cwd', cwd, 'conversation', json(conversation), 'task', json(task), \
                    'goal', json(goal), 'resume', json(resume))) \
                    FROM (SELECT * FROM sessions ORDER BY position)";
        let json = self.query(list).unwrap_or_default();
        if json == "[]" { String::new() } else { json }
    }

    /// The one value `sql` reads from the daemon's database, when there's
    /// a database to read.
    fn query(&self, sql: &str) -> Option<String> {
        let file = self.socket.with_extension("db");
        if !file.exists() {
            return None;
        }
        let conn = rusqlite::Connection::open(file).ok()?;
        conn.busy_timeout(Duration::from_secs(5)).ok()?;
        conn.query_row(sql, [], |row| row.get(0)).ok()
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
fn a_session_that_can_t_start_again_after_a_crash_stays_in_its_place_saying_why() {
    let crystal = Crystal::new();
    let daemon = crystal.start_daemon();
    let gone = crystal.dir.path().join("gone");
    std::fs::create_dir(&gone).unwrap();
    let out = crystal
        .command(&["new", "-n", "lost", "sleep", "300"])
        .current_dir(&gone)
        .output()
        .unwrap();
    assert!(out.status.success());
    crystal.ok(&["new", "-n", "keeper", "sleep", "300"]);
    eventually("both are saved", || {
        let saved = crystal.saved();
        saved.contains("lost") && saved.contains("keeper")
    });

    crash(daemon);
    std::fs::remove_dir(&gone).unwrap();
    // Left behind by the crash, it would pass for the next daemon's.
    std::fs::remove_file(&crystal.socket).unwrap();
    let daemon = crystal.start_daemon();

    // Not dropped, nor started in another directory: it stays first, and
    // says why.
    let listed = crystal.ok(&["ls"]);
    let names: Vec<&str> = listed.lines().skip(1).map(|line| &line[..6]).collect();
    assert_eq!(names, ["lost  ", "keeper"]);
    assert_eq!(status(&crystal, "lost"), "couldn't start");
    assert_eq!(status(&crystal, "keeper"), "running");
    let ls = crystal.run(&["ls"]);
    let why = String::from_utf8(ls.stderr).unwrap();
    assert!(
        why.contains("lost couldn't start again: its directory, ")
            && why.contains("gone, isn't there"),
        "{why}"
    );
    shows_on_screen(
        &crystal,
        "lost",
        "crystal couldn't start lost again after the restart",
    );
    eventually("the log says what came back and what didn't", || {
        let events = crystal.ok(&["events"]);
        events.contains("session.start_failed  lost  couldn't start: its directory")
            && events.contains("started cold: 1 session back, 1 couldn't start: lost")
    });
    // It's still written down, to start again with the next restart.
    eventually("it's still saved", || {
        let saved = crystal.saved();
        saved.contains("lost") && saved.contains("keeper")
    });

    // Started again once its directory is back, in its place and under
    // the same id.
    let err = crystal.fails(&["respawn", "lost"]);
    assert!(err.contains("isn't there"), "{err}");
    let id = |name: &str| {
        let listed: serde_json::Value =
            serde_json::from_str(&crystal.ok(&["ls", "--json"])).unwrap();
        let session = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name);
        session.unwrap()["id"].as_str().unwrap().to_string()
    };
    let before = id("lost");
    std::fs::create_dir(&gone).unwrap();
    crystal.ok(&["respawn", "lost"]);
    assert_eq!(status(&crystal, "lost"), "running");
    assert_eq!(id("lost"), before);
    let listed = crystal.ok(&["ls"]);
    assert!(
        listed.lines().nth(1).unwrap().starts_with("lost "),
        "{listed}"
    );
    crash(daemon);
}

#[test]
fn agents_start_again_a_moment_apart_after_a_crash() {
    let crystal = Crystal::new();
    crystal.configure(
        "notify = false\nname_from_prompt = false\n\n[plugins]\nmemory = false\n\n\
         [sound]\nenabled = false\n\n[sessions]\nrestart_spacing_ms = 1500\n",
    );
    let bin = fake_claude(crystal.dir.path());
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let daemon = crystal.start_daemon();
    for name in ["first", "second"] {
        let out = crystal
            .command(&["new", "-n", name, "claude"])
            .env("PATH", &path)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    crystal.ok(&["new", "-n", "shell", "sleep", "300"]);
    eventually("all three are saved", || crystal.saved().contains("shell"));

    crash(daemon);
    let restarted = Instant::now();
    // The next daemon starts the sessions again from this command's
    // environment, which finds the fake claude.
    let out = crystal
        .command(&["new", "-n", "fresh", "sleep", "300"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    // The first agent and the shell are back as soon as the daemon answers;
    // the second agent waits its turn.
    assert_eq!(status(&crystal, "first"), "running");
    assert_eq!(status(&crystal, "shell"), "running");
    assert_eq!(status(&crystal, "second"), "starting");
    // Attached meanwhile, its screen says so, then follows it as it starts.
    let attached = crystal.attach(&["attach", "second"]);
    attached.shows("starting second again after crystal's restart");
    eventually("the second agent has started", || {
        status(&crystal, "second") == "running"
    });
    assert!(restarted.elapsed() >= Duration::from_millis(1500));
    attached.hides("starting second again");
    eventually("the log says they're all back", || {
        crystal
            .ok(&["events"])
            .contains("started cold: 3 sessions back")
    });
}

#[test]
fn the_tui_says_what_a_restart_couldn_t_start_and_starts_it_again() {
    let crystal = Crystal::new();
    let daemon = crystal.start_daemon();
    let gone = crystal.dir.path().join("gone");
    std::fs::create_dir(&gone).unwrap();
    let out = crystal
        .command(&["new", "-n", "lost", "sleep", "300"])
        .current_dir(&gone)
        .output()
        .unwrap();
    assert!(out.status.success());
    eventually("it's saved", || crystal.saved().contains("lost"));
    crash(daemon);
    std::fs::remove_dir(&gone).unwrap();
    // Left behind by the crash, it would pass for the next daemon's.
    std::fs::remove_file(&crystal.socket).unwrap();
    let daemon = crystal.start_daemon();

    let mut tui = crystal.tui();
    tui.shows("after the restart: 1 couldn't start: lost");
    tui.shows("couldn't start");
    tui.shows("isn't there");
    // Put right, Enter starts it again.
    std::fs::create_dir(&gone).unwrap();
    tui.type_keys("\r");
    tui.shows("start lost again? y/n");
    tui.type_keys("y");
    eventually("it has started", || status(&crystal, "lost") == "running");
    crash(daemon);
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
    let settings: serde_json::Value = claude_settings(&args);
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

    // Started again with crystal's resume in place of its own --continue,
    // and crystal's notes in its system prompt as before, but not its
    // first prompt again.
    let args = written(&crystal.dir.path().join("args"));
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(
        args[4..7],
        ["--resume", "abc-123", "--append-system-prompt"]
    );
    assert!(
        args[7].starts_with("You're running inside crystal"),
        "{args:?}"
    );
    assert!(!args.contains(&"--"), "{args:?}");
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
    // spinner in its title while it works for a second, then answers, and
    // the spinner goes: its answer is on screen once its turn is done.
    let script = r#"
        while read line; do
            printf '\033]0;⠋ working\007'
            sleep 1
            echo "answer to $line"
            printf '\033]0;\007'
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

/// Starts a session called `name` that runs the shell script `script`,
/// then sleeps, with `$C` the crystal these tests run: a session's own
/// crystal commands go to its daemon, and say they come from it.
fn in_session(crystal: &Crystal, name: &str, script: &str) {
    let script = format!("C='{CRYSTAL}'; {script}; sleep 30");
    crystal.ok(&["new", "-n", name, "sh", "-c", &script]);
}

/// What's in `file` once it has `lines` lines.
fn once_lines(file: &Path, lines: usize) -> String {
    eventually(&format!("{} has {lines} lines", file.display()), || {
        std::fs::read_to_string(file).is_ok_and(|text| text.lines().count() == lines)
    });
    std::fs::read_to_string(file).unwrap()
}

#[test]
fn a_message_from_another_session_says_which_sent_it() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    crystal.ok(&["new", "-n", "inbox", "sh", "-c", "cat > got"]);

    in_session(
        &crystal,
        "scout",
        r#""$C" send inbox 'the codec moved' 2> err; echo $? > sent"#,
    );
    assert_eq!(written(&dir.join("sent")), "0\n");
    assert_eq!(
        once_lines(&dir.join("got"), 2),
        "[crystal] Message from session \"scout\":\nthe codec moved\n"
    );
    let logged = events(&crystal, &["-k", "session.message"]);
    assert_eq!(logged[0]["session"]["name"], "inbox");
    assert_eq!(logged[0]["message"]["from"], "scout");
    assert_eq!(logged[0]["message"]["line"], "the codec moved");

    // From a shell, it goes as typed, from nobody.
    crystal.ok(&["send", "inbox", "plain"]);
    assert!(once_lines(&dir.join("got"), 3).ends_with("moved\nplain\n"));
    let logged = events(&crystal, &["-k", "session.message"]);
    assert!(logged[1]["message"].get("from").is_none(), "{}", logged[1]);
    assert_eq!(logged[1]["message"]["line"], "plain");

    // A session can't send to itself.
    in_session(
        &crystal,
        "loner",
        r#""$C" send loner hi 2> loner-err; echo $? > loner-sent"#,
    );
    assert_eq!(written(&dir.join("loner-sent")), "1\n");
    let err = std::fs::read_to_string(dir.join("loner-err")).unwrap();
    assert!(err.contains("loner is this session"), "{err}");
}

#[test]
fn a_session_sends_twenty_messages_a_minute_at_most() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    crystal.ok(&["new", "-n", "inbox", "sh", "-c", "cat > /dev/null"]);

    in_session(
        &crystal,
        "chatty",
        r#"for i in $(seq 21); do "$C" send --no-enter inbox "m$i" 2>> errors || echo "$i" >> refused; done; echo > finished"#,
    );
    written(&dir.join("finished"));
    assert_eq!(
        std::fs::read_to_string(dir.join("refused")).unwrap(),
        "21\n"
    );
    let errors = std::fs::read_to_string(dir.join("errors")).unwrap();
    assert!(
        errors.contains("this session has sent 20 messages in the last minute"),
        "{errors}"
    );
    // You aren't held to it.
    crystal.ok(&["send", "--no-enter", "inbox", "from you"]);
}

#[test]
fn send_reads_its_text_from_stdin_and_a_bare_ok_between_sessions_is_refused() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    crystal.ok(&["new", "-n", "inbox", "sh", "-c", "cat > got"]);

    let mut send = crystal
        .command(&["send", "inbox", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    send.stdin
        .take()
        .unwrap()
        .write_all(b"from a pipe")
        .unwrap();
    assert!(send.wait().unwrap().success());
    assert_eq!(once_lines(&dir.join("got"), 1), "from a pipe\n");

    in_session(
        &crystal,
        "polite",
        r#""$C" send inbox 'ok, thanks!' 2> err; echo $? > sent; "$C" send inbox 'ok, 3 tests fail'"#,
    );
    assert_eq!(written(&dir.join("sent")), "1\n");
    let err = std::fs::read_to_string(dir.join("err")).unwrap();
    assert!(err.contains("a message that only acknowledges"), "{err}");
    assert!(
        once_lines(&dir.join("got"), 3)
            .ends_with("[crystal] Message from session \"polite\":\nok, 3 tests fail\n")
    );
    // From you, it goes as typed.
    crystal.ok(&["send", "inbox", "ok"]);
    assert!(once_lines(&dir.join("got"), 4).ends_with("fail\nok\n"));
}

#[test]
fn send_interrupt_stops_a_task_s_run_and_carries_on_from_there() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    start_task(&crystal, &path, "fixer", "go SLOW");
    eventually("the task is working", || {
        status(&crystal, "fixer") == "working"
    });

    crystal.ok(&["send", "--interrupt", "fixer", "do this instead"]);
    assert_eq!(
        runs(dir, 2)[1],
        format!("{PRINT_ARGS} --allowedTools {ALLOWED} -- do this instead")
    );
    let logged = events(&crystal, &["-n", "fixer", "-k", "run.interrupted"]);
    assert_eq!(logged.len(), 1);
    finish_run(dir, 2);
    eventually("the task is done", || status(&crystal, "fixer") == "done");
    // A task between runs is sent to as ever.
    crystal.ok(&["send", "--interrupt", "fixer", "and the docs"]);
    assert_eq!(runs(dir, 3).len(), 3);

    crystal.ok(&["new", "-n", "term", "sleep", "30"]);
    let refused = crystal.fails(&["send", "--interrupt", "term", "stop"]);
    assert!(refused.contains("term isn't a task"), "{refused}");
    assert!(
        refused.contains("crystal send-keys term Escape"),
        "{refused}"
    );
}

#[test]
fn a_wait_that_gives_up_exits_2_and_quiet_prints_nothing() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "busy", "sleep", "30"]);
    let code = |args: &[&str]| crystal.run(args).status.code();

    assert_eq!(code(&["wait", "busy", "--timeout", "0.2"]), Some(2));
    assert_eq!(
        code(&["wait", "busy", "--until", "waiting", "--timeout", "0.2"]),
        Some(2)
    );
    let out = crystal.run(&["wait", "busy", "--output", "never", "--timeout", "0.2"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("nothing on busy's screen matched `never`"),
        "{err}"
    );
    // Anything else that goes wrong is 1, a mistyped flag included, so 2
    // always means not yet.
    assert_eq!(code(&["wait", "busy", "--nope"]), Some(1));
    assert_eq!(code(&["wait", "nobody", "--timeout", "0.2"]), Some(1));

    crystal.ok(&["new", "-n", "says", "sh", "-c", "echo ready; sleep 30"]);
    assert_eq!(crystal.ok(&["wait", "says", "--output", "ready", "-q"]), "");
    crystal.ok(&["new", "-n", "quick", "true"]);
    assert_eq!(crystal.ok(&["wait", "quick", "--quiet"]), "");
    assert_eq!(crystal.ok(&["wait", "quick"]), "exited 0\n");
}

#[test]
fn read_keeps_colors_joins_wrapped_lines_and_reads_what_came_since() {
    let crystal = Crystal::new();
    let script = r#"printf '\033[31mred\033[0m plain\n'; printf '%0150d\n' 7; sleep 3; echo later; sleep 30"#;
    crystal.ok(&["new", "-n", "printer", "sh", "-c", script]);
    shows_on_screen(&crystal, "printer", "later");
    // Right after it: the first lines came 3s before.
    let since = crystal.ok(&["read", "printer", "--since", "2s"]);
    assert_eq!(since, "later\n");

    let ansi = crystal.ok(&["read", "printer", "--ansi"]);
    assert!(
        ansi.starts_with("\x1b[0;38;5;1mred\x1b[0m plain\n"),
        "{ansi:?}"
    );
    let plain = crystal.ok(&["read", "printer"]);
    assert!(plain.starts_with("red plain\n"), "{plain:?}");
    // The 150 digits wrapped onto two rows of the 120 the screen has.
    let digits = format!("{:0150}", 7);
    assert!(!plain.contains(&digits), "{plain}");
    let unwrapped = crystal.ok(&["read", "printer", "--unwrap"]);
    assert!(unwrapped.contains(&format!("{digits}\n")), "{unwrapped}");

    // Since before the program began, it's all of it.
    let all = crystal.ok(&["read", "printer", "--since", "1h"]);
    assert!(
        all.starts_with("red plain\n") && all.ends_with("later\n"),
        "{all}"
    );
    let err = crystal.fails(&["read", "printer", "--since", "yesterday"]);
    assert!(err.contains("nor a time"), "{err}");
}

#[test]
fn events_keep_to_a_task_and_the_newest_few() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    crystal.ok(&["new", "-n", "bystander", "sleep", "30"]);
    start_task(&crystal, &path, "fixer", "go");
    finish_run(dir, 1);
    eventually("the task is done", || status(&crystal, "fixer") == "done");

    let task = events(&crystal, &["--task", "t1"]);
    assert!(!task.is_empty());
    for event in &task {
        let about_it = event["task"]["id"] == 1 || event["session"]["task_id"] == 1;
        assert!(about_it, "{event}");
    }
    let kinds = names(&task);
    assert!(kinds.contains(&"task.opened".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"run.started".to_string()), "{kinds:?}");
    assert_eq!(events(&crystal, &["--task", "12"]).len(), 0);
    let err = crystal.fails(&["events", "--task", "twelve"]);
    assert!(err.contains("isn't a task's number"), "{err}");

    let all = events(&crystal, &[]);
    let newest = events(&crystal, &["--limit", "2"]);
    assert_eq!(newest, all[all.len() - 2..]);
    let lines = crystal.ok(&["events", "-l", "1"]);
    assert_eq!(lines.lines().count(), 1);
}

#[test]
fn process_info_lists_what_is_in_front_and_where_it_works() {
    let crystal = Crystal::new();
    let sub = crystal.dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    crystal.ok(&["new", "-n", "box", "sh", "-c", "cd sub && exec sleep 30"]);
    eventually("sleep is in front", || {
        crystal.ok(&["process-info", "box"]).contains("sleep")
    });
    let table = crystal.ok(&["ps", "box"]);
    let mut lines = table.lines();
    assert!(lines.next().unwrap().starts_with("PID"), "{table}");
    let row = lines.next().unwrap();
    assert!(
        row.contains("sleep") && row.ends_with("sleep 30"),
        "{table}"
    );

    let info: serde_json::Value =
        serde_json::from_str(&crystal.ok(&["process-info", "box", "--json"])).unwrap();
    let front = &info["foreground"][0];
    assert_eq!(front["name"], "sleep");
    assert_eq!(front["argv"], serde_json::json!(["sleep", "30"]));
    assert_eq!(info["group"], front["pid"]);
    let cwd = PathBuf::from(front["cwd"].as_str().unwrap());
    assert_eq!(cwd.canonicalize().unwrap(), sub.canonicalize().unwrap());
}

#[test]
fn crystal_agent_names_the_agent_a_wrapper_runs() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "plain", "sleep", "30"]);
    // A wrapper of the user's own: a Mac keeps the environment of the
    // programs it ships, like sleep, to itself. crystal waiting on another
    // session stands in for one.
    let wrapper = format!("CRYSTAL_AGENT=claude exec '{CRYSTAL}' wait plain");
    crystal.ok(&["new", "-n", "wrapped", "sh", "-c", &wrapper]);
    let front = |name: &str| -> serde_json::Value {
        let sessions: serde_json::Value =
            serde_json::from_str(&crystal.ok(&["ls", "--json"])).unwrap();
        let session = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["name"] == name)
            .unwrap()
            .clone();
        session["front"].clone()
    };
    eventually("the wrapper reads as Claude Code", || {
        front("wrapped")["program"] == "claude"
    });
    assert_eq!(front("wrapped")["kind"], "agent");
    eventually("sleep reads as itself", || {
        front("plain")["name"] == "sleep"
    });
}

/// The bytes `text`, base64 with or without padding, stands for.
fn unbase64(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = Vec::new();
    let (mut bits, mut held) = (0u32, 0);
    for c in text.trim_end_matches('=').bytes() {
        let value = ALPHABET.iter().position(|&a| a == c).unwrap() as u32;
        bits = (bits << 6) | value;
        held += 6;
        if held >= 8 {
            held -= 8;
            bytes.push((bits >> held) as u8);
            bits &= (1 << held) - 1;
        }
    }
    bytes
}

/// What a stream's `output` lines have carried so far, as text.
fn streamed(lines: &std::sync::mpsc::Receiver<String>, until: &str) -> String {
    let mut text = String::new();
    while !text.contains(until) {
        let line: serde_json::Value = serde_json::from_str(&next_line(lines)).unwrap();
        if line["type"] == "output" {
            text.push_str(&String::from_utf8_lossy(&unbase64(
                line["data"].as_str().unwrap(),
            )));
        }
    }
    text
}

#[test]
fn observe_streams_a_session_as_json_and_control_drives_it() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "echoer", "cat"]);

    // Observing it changes nothing: not its size, which no viewer has set.
    let mut observe = crystal
        .command(&["observe", "echoer"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let observed = lines_of(&mut observe);
    let start: serde_json::Value = serde_json::from_str(&next_line(&observed)).unwrap();
    assert_eq!(start["type"], "start");
    assert_eq!(start["session"], "echoer");
    assert_eq!(
        (start["rows"].clone(), start["cols"].clone()),
        (40.into(), 120.into())
    );

    let mut control = crystal
        .command(&["control", "echoer", "--rows", "10", "--cols", "50"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let controlled = lines_of(&mut control);
    let start: serde_json::Value = serde_json::from_str(&next_line(&controlled)).unwrap();
    assert_eq!(
        (start["rows"].clone(), start["cols"].clone()),
        (10.into(), 50.into())
    );
    let mut commands = control.stdin.take().unwrap();
    commands
        .write_all(b"{\"type\":\"input\",\"text\":\"hello there\\r\"}\n")
        .unwrap();
    assert!(streamed(&controlled, "hello there").contains("hello there"));
    assert!(streamed(&observed, "hello there").contains("hello there"));
    commands.write_all(b"not json\n").unwrap();
    let error: serde_json::Value = loop {
        let line: serde_json::Value = serde_json::from_str(&next_line(&controlled)).unwrap();
        if line["type"] == "error" {
            break line;
        }
    };
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("isn't a command"),
        "{error}"
    );

    // Keys by name: C-c ends cat, and both streams with it.
    commands
        .write_all(b"{\"type\":\"keys\",\"keys\":[\"C-c\"]}\n")
        .unwrap();
    for lines in [&controlled, &observed] {
        let closed: serde_json::Value = loop {
            let line: serde_json::Value = serde_json::from_str(&next_line(lines)).unwrap();
            if line["type"] == "closed" {
                break line;
            }
        };
        assert_eq!(closed["reason"], "ended");
    }
    assert!(control.wait().unwrap().success());
    assert!(observe.wait().unwrap().success());
}

#[test]
fn control_lets_go_when_told_or_when_its_input_ends() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "echoer", "cat"]);
    let mut control = crystal
        .command(&["control", "echoer"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let lines = lines_of(&mut control);
    next_line(&lines);
    control
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"type\":\"release\"}\n")
        .unwrap();
    let closed: serde_json::Value = loop {
        let line: serde_json::Value = serde_json::from_str(&next_line(&lines)).unwrap();
        if line["type"] == "closed" {
            break line;
        }
    };
    assert_eq!(closed["reason"], "released");
    assert!(control.wait().unwrap().success());
    assert_eq!(status(&crystal, "echoer"), "running");

    let out = crystal
        .command(&["control", "echoer"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success());
    let said = String::from_utf8(out.stdout).unwrap();
    assert!(
        said.ends_with("{\"type\":\"closed\",\"reason\":\"released\"}\n"),
        "{said}"
    );
}

#[test]
fn api_snapshot_holds_everything_and_the_seq_to_follow_on_from() {
    let crystal = Crystal::new();
    let err = crystal.fails(&["api", "snapshot"]);
    assert!(err.contains("no daemon is running"), "{err}");
    crystal.ok(&["new", "-n", "first", "sleep", "30"]);

    let snapshot: serde_json::Value =
        serde_json::from_str(&crystal.ok(&["api", "snapshot"])).unwrap();
    assert_eq!(snapshot["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(snapshot["sessions"][0]["name"], "first");
    assert_eq!(snapshot["sessions"][0]["status"], "running");
    assert_eq!(snapshot["layout"]["tabs"][0]["sessions"][0], "first");
    for list in ["projects", "tasks", "flows", "archived"] {
        assert!(snapshot[list].is_array(), "{list}: {snapshot}");
    }
    let seq = snapshot["seq"].as_u64().unwrap();
    assert!(seq > 0, "{snapshot}");

    let after = seq.to_string();
    assert_eq!(crystal.ok(&["events", "--after", &after]), "");
    crystal.ok(&["new", "-n", "second", "sleep", "30"]);
    let news = events(&crystal, &["--after", &after]);
    assert_eq!(names(&news), ["session.started"]);
    assert_eq!(news[0]["session"]["name"], "second");
}

#[test]
fn integration_status_lists_only_those_out_of_date_when_asked() {
    let crystal = Crystal::new();
    std::fs::create_dir_all(crystal.claude_config_dir()).unwrap();
    crystal.integration(&["install", "claude"]).unwrap();
    assert_eq!(
        crystal.integration(&["status", "--outdated-only"]).unwrap(),
        ""
    );

    // An earlier crystal listened to fewer events.
    let file = crystal.claude_config_dir().join("settings.json");
    let mut settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    settings["hooks"]
        .as_object_mut()
        .unwrap()
        .remove("SubagentStop");
    std::fs::write(&file, settings.to_string()).unwrap();
    let outdated = crystal.integration(&["status", "--outdated-only"]).unwrap();
    assert!(outdated.starts_with("claude  out of date  "), "{outdated}");
    assert_eq!(outdated.lines().count(), 1, "{outdated}");

    crystal.integration(&["install", "claude"]).unwrap();
    assert_eq!(
        crystal.integration(&["status", "--outdated-only"]).unwrap(),
        ""
    );
}

#[test]
fn the_settings_view_puts_an_agent_s_hooks_in_and_takes_them_out() {
    let crystal = Crystal::new();
    // The TUI looks for agents in a home of the test's own: Claude Code is
    // the only one there.
    std::fs::create_dir_all(crystal.claude_config_dir()).unwrap();
    let mut tui = crystal.tui();
    tui.type_keys(",");
    tui.shows("General");
    // The integrations' tab: Claude Code, the only agent installed here.
    tui.type_keys("7");
    tui.shows("crystal's hooks in each agent's own settings");
    tui.shows("Claude Code");
    tui.type_keys(" ");
    let file = crystal.claude_config_dir().join("settings.json");
    eventually("the hooks are in", || {
        std::fs::read_to_string(&file).is_ok_and(|text| text.contains("hook claude --installed"))
    });
    // The view reads them again, and marks the row.
    tui.shows("● Claude Code");
    tui.type_keys(" ");
    eventually("the hooks are out", || {
        std::fs::read_to_string(&file).is_ok_and(|text| !text.contains("hook claude"))
    });
}

#[test]
fn the_tui_follows_the_config_file_as_it_changes() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);
    let mut tui = crystal.tui();
    tui.shows("agent");
    let config = std::fs::read_to_string(crystal.config_file()).unwrap();

    // Set by hand, the tab bar goes where it says, without a restart.
    let moved = format!("{config}\n[tab_bar]\nposition = \"bottom\"\nhide_when_single = false\n");
    std::fs::write(crystal.config_file(), &moved).unwrap();
    tui.shows("the config file changed: the settings follow it");

    // A file that makes no sense is said, and the settings stay.
    std::fs::write(crystal.config_file(), "notfy = true\n").unwrap();
    tui.shows("the settings stay as they were");
    tui.type_keys(",");
    tui.shows("settings");
}

#[test]
fn send_refuses_an_agent_asking_something_unless_forced() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    crystal.ok(&["new", "-n", "agent", "sh", "-c", "cat > got"]);
    crystal.ok(&[
        "report",
        "-n",
        "agent",
        "--agent",
        "pi",
        "waiting",
        "-m",
        "Allow cargo test?",
    ]);

    let refused = crystal.fails(&["send", "agent", "carry on"]);
    assert!(
        refused.contains(
            "agent_blocked: agent is waiting on the user (Allow cargo test?). Answer it first, \
             in its pane, or with `crystal send-keys agent …`, or send again with --force"
        ),
        "{refused}"
    );
    // Keys are how a question is answered: they're never refused.
    crystal.ok(&["send-keys", "agent", "y", "Enter"]);
    crystal.ok(&["send", "--force", "agent", "carry on"]);
    assert_eq!(once_lines(&dir.join("got"), 2), "y\ncarry on\n");

    // At its prompt again, it takes what it's sent.
    crystal.ok(&["report", "-n", "agent", "idle"]);
    crystal.ok(&["send", "agent", "next"]);
    assert_eq!(once_lines(&dir.join("got"), 3), "y\ncarry on\nnext\n");
}

/// Whether `text` holds anything a terminal would take as an order: a
/// control character but a line break, or a bidi control.
fn holds_orders(text: &str) -> bool {
    let bidi = |c: char| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    text.chars()
        .any(|c| (c.is_control() && c != '\n') || bidi(c))
}

#[test]
fn what_agents_say_reaches_the_terminal_as_text_alone() {
    let crystal = Crystal::new();
    let refused = crystal.fails(&["new", "-n", "evil\x1b]0;pwned\x07", "sh"]);
    assert!(
        refused.contains("can't contain control characters"),
        "{refused}"
    );
    assert!(!holds_orders(&refused), "{refused:?}");

    crystal.ok(&["new", "-n", "agent", "sh", "-c", "sleep 30"]);
    let message = "Allow\x1b]52;c;aGk=\x07 it?\u{202e}";
    crystal.ok(&[
        "report", "-n", "agent", "--agent", "pi", "waiting", "-m", message,
    ]);
    let refused = crystal.fails(&["send", "agent", "carry on"]);
    assert!(refused.contains("(Allow]52;c;aGk= it?)"), "{refused}");
    assert!(!holds_orders(&refused), "{refused:?}");
    let events = crystal.ok(&["events"]);
    assert!(events.contains("Allow]52;c;aGk= it?"), "{events}");
    assert!(!holds_orders(&events), "{events:?}");

    crystal.ok(&["backlog", "add", "Retry\x1b[?1049h\r\nthe \u{9b}2Jwebhook"]);
    let backlog = crystal.ok(&["backlog"]);
    assert!(backlog.contains("Retry[?1049h"), "{backlog}");
    assert!(!holds_orders(&backlog), "{backlog:?}");
}

/// A stand-in for a background task's `claude -p` that answers every
/// prompt with orders for the terminal in all it says: its text (and a
/// character reference markdown reads as ESC), a tool's name and what it
/// was asked, the tool's answer, and its result.
fn hostile_claude(dir: &Path) -> PathBuf {
    let bin = dir.join("hostile-bin");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join("claude"),
        r#"while IFS= read -r line; do
    case "$line" in *'"type":"user"'*) ;; *) continue ;; esac
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"conv-1","cwd":"/x","model":"m"}'
    printf '%s\n' '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Said \u001b]0;pwned\u0007 then \u001b]52;c;aGk=\u0007 then \u001b[?1049h then &#x1b;]2;ref&#7; end"},{"type":"tool_use","id":"t1","name":"Bash\u001b[2J","input":{"command":"ls \u009b2J here"}}]}}'
    printf '%s\n' '{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"out \u001b]8;;https://evil.example\u001b\\link\u001b]8;;\u001b\\ \u202eabc","is_error":false}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done \u001b]0;r\u0007","session_id":"conv-1","total_cost_usd":0.01,"duration_ms":100}'
done
"#,
    );
    bin
}

#[test]
fn a_task_s_screen_draws_what_claude_says_and_takes_no_orders_from_it() {
    let crystal = Crystal::new();
    let path = path_with(&hostile_claude(crystal.dir.path()));
    let out = crystal
        .command(&["task", "-n", "hostile", "look", "around"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    eventually("the task is done", || status(&crystal, "hostile") == "done");
    // Had the screen taken them as orders, the sequences would be gone,
    // and the alternate screen would hide everything drawn before it.
    shows_on_screen(
        &crystal,
        "hostile",
        "Said ]0;pwned then ]52;c;aGk= then [?1049h then ]2;ref end",
    );
    // Not Bash's name, so what it was asked shows whole.
    shows_on_screen(&crystal, "hostile", r#"▸ Bash[2J {"command":"ls 2J here"}"#);
    shows_on_screen(
        &crystal,
        "hostile",
        "└ out ]8;;https://evil.example\\link]8;;\\ abc",
    );
    let result = crystal.ok(&["result", "hostile"]);
    assert_eq!(result, "done ]0;r\n");
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

    // Back on alpha, the selection's pane goes on showing beta, and s
    // closes alpha's split: beta leaves the screen.
    tui.type_keys("k");
    tui.shows("alpha · selected");
    tui.shows("beta is here");
    tui.type_keys("s");
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

    // reader's split stays on top, where it was: the first Tab goes to it,
    // the next to the selection's pane below, and the next round to it
    // again.
    tui.type_keys("\t");
    tui.shows("typing into reader");
    tui.type_keys("\x1c");
    tui.shows("q quit");
    tui.type_keys("\t");
    tui.shows("typing into other");
    tui.type_keys("\x1c");
    tui.shows("q quit");
    tui.type_keys("\t");
    tui.shows("typing into reader");
    tui.type_keys("hello split\r");

    assert_eq!(written(&crystal.dir.path().join("got")), "hello split\n");
}

/// The row of the screen `text` first shows on, counted from 0.
fn row_of(tui: &Terminal, text: &str) -> Option<usize> {
    tui.text().lines().position(|row| row.contains(text))
}

#[test]
fn capital_k_moves_a_pane_up_past_the_one_above_and_the_tab_keeps_it_there() {
    let crystal = Crystal::new();
    for name in ["alpha", "beta"] {
        let script = format!("echo {name} is here; echo > {name}-ready; sleep 30");
        crystal.ok(&["new", "-n", name, "sh", "-c", &script]);
        written(&crystal.dir.path().join(format!("{name}-ready")));
    }

    // At 80 columns s splits alpha off above the selection's pane, where
    // beta shows.
    let mut tui = crystal.tui();
    tui.shows("alpha is here");
    tui.type_keys("sj");
    tui.shows("beta is here");
    let above = |tui: &Terminal, first: &str, second: &str| matches!((row_of(tui, first), row_of(tui, second)), (Some(a), Some(b)) if a < b);
    assert!(
        above(&tui, "alpha is here", "beta is here"),
        "{}",
        tui.text()
    );

    tui.type_keys("K");
    eventually("beta's pane goes above alpha's", || {
        above(&tui, "beta is here", "alpha is here")
    });

    // The panes are the tab's: they're there again when the TUI opens.
    tui.type_keys("q");
    assert!(tui.exit());
    let tui = crystal.tui();
    tui.shows("beta is here");
    tui.shows("alpha is here");
    assert!(
        above(&tui, "beta is here", "alpha is here"),
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
    // beta goes, and the layout starts it again.
    crystal.ok(&["kill", "beta"]);
    sidebar_hides(&tui, "beta");

    tui.type_keys("S");
    tui.shows("side by side");
    tui.type_keys("\r");
    tui.shows("restored side by side: one of its sessions started again");
    tui.shows("1 review");
    tui.shows("alpha is here");
    tui.shows("beta is here");

    // The tabs it replaced are kept, to go back to.
    tui.type_keys("S");
    tui.shows("↶ before side by side");
    let layouts = crystal.query("SELECT json FROM ui WHERE name = 'layouts'");
    assert!(layouts.unwrap().contains("side by side"));
}

#[test]
fn a_session_nobody_has_looked_at_is_120_columns_by_40_rows() {
    let crystal = Crystal::new();
    crystal.ok(&[
        "new",
        "-n",
        "unseen",
        "sh",
        "-c",
        "stty size > size; sleep 30",
    ]);
    let file = crystal.dir.path().join("size");
    eventually("the session says its size", || {
        std::fs::read_to_string(&file).is_ok_and(|size| size == "40 120\n")
    });
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
    // side by side, so s stacks the panes: each 51 columns wide, with the
    // 22 rows between the top bar and the footer shared between them, less
    // a header line each.
    eventually("the panes are stacked", || {
        let (left, right) = (size_of("left"), size_of("right"));
        left.1 == 51 && right.1 == 51 && left.0 + right.0 + 2 == 22
    });

    // Wider, they stay stacked, each as wide as the room beside the
    // sidebar, 171 columns, and the 28 rows shared between them.
    tui.resize(30, 200);
    eventually("the panes widen", || {
        size_of("left") == (13, 171) && size_of("right") == (13, 171)
    });

    // Split off again at that width, left goes beside the selection's
    // pane: two panes of 85, with a rule between them.
    tui.type_keys("kssj");
    eventually("the panes are side by side", || {
        size_of("left") == (27, 85) && size_of("right") == (27, 85)
    });
}

/// Starts a session for each of `names` that writes its size to a file
/// called after it whenever it changes.
fn sessions_telling_their_size(crystal: &Crystal, names: &[&str]) {
    for name in names {
        let script = format!(
            "trap 'stty size > {name}-size' WINCH; echo {name} watching; \
             while :; do sleep 0.05; done"
        );
        crystal.ok(&["new", "-n", name, "sh", "-c", &script]);
    }
}

/// The `(rows, columns)` the session called `name` last said it has.
fn told_size(crystal: &Crystal, name: &str) -> (u16, u16) {
    let file = crystal.dir.path().join(format!("{name}-size"));
    let size = std::fs::read_to_string(file).unwrap_or_default();
    let mut numbers = size.split_whitespace().map(|n| n.parse().unwrap_or(0));
    (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0))
}

#[test]
fn bar_and_dash_split_any_pane_and_each_session_is_sized_to_its_own() {
    let crystal = Crystal::new();
    sessions_telling_their_size(&crystal, &["one", "two", "three"]);
    let mut tui = crystal.tui();
    tui.shows("one watching");

    // one | (two over three): 51 columns beside the sidebar make two of 25
    // and a rule; two and three share 22 rows, less their header lines.
    tui.type_keys("|j");
    tui.shows("two watching");
    tui.type_keys("-j");
    tui.shows("three watching");
    eventually("each session is its pane's size", || {
        told_size(&crystal, "one") == (21, 25)
            && told_size(&crystal, "two") == (10, 25)
            && told_size(&crystal, "three") == (10, 25)
    });
    tui.shows("one watching");
    tui.shows("two watching");

    // Shift and an arrow go from pane to pane.
    tui.type_keys("\x1b[1;2D");
    tui.shows("one · selected");
}

#[test]
fn r_resizes_the_selected_pane_with_the_keys_and_a_rule_drags_with_the_mouse() {
    let crystal = Crystal::new();
    sessions_telling_their_size(&crystal, &["left", "right"]);
    let mut tui = crystal.tui();
    tui.shows("left watching");
    tui.type_keys("|j");
    eventually("the panes share the room", || {
        told_size(&crystal, "left") == (21, 25) && told_size(&crystal, "right") == (21, 25)
    });

    // right, in the selection's pane, has no border on its right: l moves
    // the one on its left, four columns at a time.
    tui.type_keys("R");
    tui.shows("resizing right");
    tui.type_keys("ll");
    eventually("the border moved right", || {
        told_size(&crystal, "left") == (21, 33) && told_size(&crystal, "right") == (21, 17)
    });
    tui.type_keys("=");
    eventually("the panes are even again", || {
        told_size(&crystal, "left") == (21, 25) && told_size(&crystal, "right") == (21, 25)
    });
    tui.type_keys("\x1b");
    tui.hides("resizing right");

    // The rule between them is at column 54. Dragged to column 40, it
    // would leave left 11 columns: it stops at 12, the fewest a pane has.
    tui.type_keys("\x1b[<0;55;10M\x1b[<32;45;10M\x1b[<32;41;10M\x1b[<0;41;10m");
    eventually("the rule went where it was dragged", || {
        told_size(&crystal, "left") == (21, 12) && told_size(&crystal, "right") == (21, 38)
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
    let settings: serde_json::Value = claude_settings(&args);
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

/// A program that counts, a line every tenth of a second.
const COUNTER: &str = "i=0; while true; do i=$((i+1)); echo count $i; sleep 0.1; done";

/// The highest count `name` has on its screen.
fn last_count(crystal: &Crystal, name: &str) -> u64 {
    crystal
        .ok(&["read", name])
        .lines()
        .filter_map(|line| line.strip_prefix("count ")?.parse().ok())
        .max()
        .unwrap_or(0)
}

#[test]
fn restart_server_hands_the_sessions_over_and_they_never_stop() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "counter", "sh", "-c", COUNTER]);
    crystal.ok(&["new", "-n", "echo", "cat"]);
    crystal.ok(&["new", "-n", "ended", "sh", "-c", "echo bye; exit 3"]);
    crystal.ok(&["send", "echo", "before"]);
    shows_on_screen(&crystal, "echo", "before");
    eventually("it has counted a while", || {
        last_count(&crystal, "counter") >= 5
    });
    eventually("the last has ended", || {
        crystal.row("ended").unwrap()[1] == "exited 3"
    });
    let (counter, echo) = (crystal.pid("counter"), crystal.pid("echo"));

    assert_eq!(
        crystal.ok(&["restart-server"]),
        "restarted the daemon, and its sessions carried on\n"
    );
    // The same programs, never stopped, their screens as they were.
    assert_eq!(crystal.pid("counter"), counter);
    assert_eq!(crystal.pid("echo"), echo);
    shows_on_screen(&crystal, "echo", "before");
    let counted = last_count(&crystal, "counter");
    eventually("it counts on", || last_count(&crystal, "counter") > counted);
    let history = crystal.ok(&["read", "counter", "--history"]);
    assert!(history.lines().any(|line| line == "count 1"), "{history}");
    // Typing still reaches them.
    crystal.ok(&["send", "echo", "after"]);
    shows_on_screen(&crystal, "echo", "after");
    // The one that had ended is there as it ended.
    assert_eq!(crystal.row("ended").unwrap()[1], "exited 3");
    shows_on_screen(&crystal, "ended", "bye");
    let events = crystal.ok(&["events"]);
    assert!(
        events.contains("daemon.handed_over  daemon  from crystal"),
        "{events}"
    );
    // A program that ends after the handover is seen to, the way it ended.
    crystal.ok(&["kill", "counter"]);
    crystal.ok(&["send-keys", "echo", "C-d"]);
    eventually("cat has ended", || {
        crystal.row("echo").unwrap()[1] == "exited 0"
    });
}

#[test]
fn two_handovers_asked_for_at_once_both_carry_the_sessions_on() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "keeper", "sleep", "300"]);
    let pid = crystal.pid("keeper");
    let restarts: Vec<std::process::Child> = (0..2)
        .map(|_| {
            crystal
                .command(&["restart-server"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for restart in restarts {
        let out = restart.wait_with_output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "restarted the daemon, and its sessions carried on\n"
        );
    }
    assert_eq!(crystal.pid("keeper"), pid);
}

#[test]
fn a_task_halfway_through_a_run_carries_on_through_a_handover() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    let task = |name: &str, prompt: &str| {
        let out = crystal
            .command(&["task", "-n", name, prompt])
            .env("PATH", &path)
            .output()
            .unwrap();
        assert!(out.status.success());
    };
    task("fixer", "fix the tests");
    // Its claude has it, as run 1.
    runs(dir, 1);
    eventually("it's working", || status(&crystal, "fixer") == "working");
    task("asker", "ASK first");
    eventually("it's asking", || status(&crystal, "asker") == "waiting");
    let pids = (crystal.pid("fixer"), crystal.pid("asker"));

    crystal.ok(&["restart-server"]);
    assert_eq!((crystal.pid("fixer"), crystal.pid("asker")), pids);
    shows_on_screen(&crystal, "fixer", "▸ Bash cargo test");
    // The run goes on in the claude it had, which is answered there too.
    finish_run(dir, 1);
    eventually("it's done", || status(&crystal, "fixer") == "done");
    assert_eq!(crystal.ok(&["result", "fixer"]), "All green on run 1.\n");
    crystal.ok(&["answer", "asker", "y"]);
    eventually("it's done", || status(&crystal, "asker") == "done");
    assert_eq!(runs(dir, 2).len(), 2, "no claude started again");
    assert!(
        std::fs::read_to_string(dir.join("answers"))
            .unwrap()
            .contains(r#""behavior":"allow""#)
    );
}

#[test]
fn a_flow_s_step_carries_on_through_a_handover() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let path = path_with(&flow_claude(dir));
    assert_eq!(
        flow_ok(&crystal, &path, &["pair", "SLOW", "down"]),
        "pair-1\n"
    );
    runs(dir, 1);

    crystal.ok(&["restart-server"]);
    let listed = crystal.ok(&["flow"]);
    assert!(!listed.contains("interrupted"), "{listed}");
    std::fs::write(dir.join("go"), "").unwrap();
    // The next step starts from the run's environment, which finds the
    // fake claude.
    assert_eq!(flow_waits(&crystal, "pair-1"), "done\n");
    assert_eq!(runs(dir, 2).len(), 2);
}

#[test]
fn an_attach_carries_on_through_a_handover() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "echo", "cat"]);
    let mut attached = crystal.attach(&["attach", "echo"]);
    attached.type_keys("before\r");
    attached.shows("before");

    crystal.ok(&["restart-server"]);
    // Shown once the attach has come back, whether it comes as output or
    // in the screen it's sent as it does.
    crystal.ok(&["send", "echo", "from outside"]);
    attached.shows("from outside");
    attached.type_keys("after\r");
    attached.shows("after");
    attached.shows("before");
    assert!(attached.child.try_wait().unwrap().is_none());
}

#[test]
fn the_tui_carries_on_through_a_handover() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "cat", "cat"]);
    let mut tui = crystal.tui();
    tui.shows("❯ cat");
    tui.type_keys("\r");
    tui.shows("typing into");
    tui.type_keys("before\r");
    tui.shows("before");

    crystal.ok(&["restart-server"]);
    crystal.ok(&["send", "cat", "from outside"]);
    tui.shows("from outside");
    tui.type_keys("after\r");
    tui.shows("after");
    tui.shows("before");
    tui.shows("❯ cat");
}

#[test]
fn following_events_carries_on_through_a_handover_with_none_missed() {
    let crystal = Crystal::new();
    // From a while back: it may only have subscribed once the first session
    // below has started.
    let mut follower = crystal
        .command(&["events", "--follow", "--since", "1h"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    thread::spawn({
        let lines = lines.clone();
        let output = follower.stdout.take().unwrap();
        move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(output).lines() {
                lines.lock().unwrap().push(line.unwrap());
            }
        }
    });
    let followed = |what: &str, name: &str| {
        let lines = lines.clone();
        let (what, name) = (what.to_string(), name.to_string());
        move || {
            let lines = lines.lock().unwrap();
            lines
                .iter()
                .any(|line| line.contains(&what) && line.contains(&name))
        }
    };
    // Followed from before the handover...
    crystal.ok(&["new", "-n", "before", "sleep", "30"]);
    eventually("it was followed", followed("session.started", "before"));

    crystal.ok(&["restart-server"]);
    crystal.ok(&["new", "-n", "after", "sleep", "30"]);
    // ...to after, the handover itself too.
    eventually(
        "the handover was followed",
        followed("daemon.handed_over", ""),
    );
    eventually("it was followed", followed("session.started", "after"));
    follower.kill().unwrap();
    follower.wait().unwrap();
}

#[test]
fn wait_output_carries_on_through_a_handover() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "echo", "cat"]);
    let waiting = crystal
        .command(&["wait", "echo", "--output", "^later$", "--timeout", "20"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // It's waiting once the daemon is looking at the screen for it.
    eventually("the daemon is looking for it", || {
        crystal.listed("echo")["output_waits"] == 1
    });
    crystal.ok(&["restart-server"]);
    crystal.ok(&["send", "echo", "later"]);
    let out = waiting.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "later\n");
}

/// Asks the daemon to hand over to the program at `exe`, the way
/// `restart-server` asks it to hand over to itself, as a crystal that reads
/// handovers of `format`; and gives back the answer, or nothing when the
/// connection closes without one.
fn ask_to_hand_over(crystal: &Crystal, exe: &Path, format: u32) -> String {
    use std::io::BufRead;
    let conn = std::os::unix::net::UnixStream::connect(&crystal.socket).unwrap();
    let request = serde_json::json!({"type": "handover", "exe": exe, "format": format});
    writeln!(&conn, "{request}").unwrap();
    let mut answer = String::new();
    let _ = std::io::BufReader::new(&conn).read_line(&mut answer);
    answer
}

/// What this crystal's handovers are written as: `handover::FORMAT`.
const HANDOVER_FORMAT: u32 = 1;

#[test]
fn a_handover_to_a_crystal_that_reads_another_kind_is_refused_and_nothing_changes() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "keeper", "sleep", "300"]);
    let pid = crystal.pid("keeper");

    let answer = ask_to_hand_over(&crystal, Path::new(CRYSTAL), HANDOVER_FORMAT + 1);
    assert!(
        answer.contains("couldn't hand over: the new crystal reads handovers of another kind"),
        "{answer}"
    );
    assert_eq!(crystal.pid("keeper"), pid);
    crystal.ok(&["new", "-n", "fresh", "sleep", "300"]);
}

#[test]
fn a_crystal_that_cant_read_its_handover_starts_the_sessions_again() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "keeper", "sleep", "300"]);
    eventually("the session is saved", || {
        crystal.saved().contains("keeper")
    });
    let old_pid = crystal.pid("keeper");
    // The new crystal is told to read the handover from a descriptor that
    // isn't open.
    let lost = crystal.dir.path().join("lost-crystal");
    script(
        &lost,
        &format!("exec {CRYSTAL} \"$1\" \"$2\" \"$3\" --handover 999\n"),
    );

    // It stops, as a daemon that crashed does, and the program with it.
    assert_eq!(ask_to_hand_over(&crystal, &lost, HANDOVER_FORMAT), "");
    eventually("the old program was hung up on", || !alive(old_pid));
    // The next daemon starts it again.
    crystal.ok(&["new", "-n", "fresh", "sleep", "300"]);
    assert_eq!(crystal.row("keeper").unwrap()[1], "running");
    assert_ne!(crystal.pid("keeper"), old_pid);
}

#[test]
fn a_daemon_from_before_handovers_is_restarted_cold() {
    use std::io::BufRead;
    let crystal = Crystal::new();
    // One that says it's another version to all but a shutdown, which it
    // answers by going, as an older crystal's does.
    let listener = std::os::unix::net::UnixListener::bind(&crystal.socket).unwrap();
    let socket = crystal.socket.clone();
    let older = thread::spawn(move || {
        for conn in listener.incoming() {
            let conn = conn.unwrap();
            let mut request = String::new();
            std::io::BufReader::new(&conn)
                .read_line(&mut request)
                .unwrap();
            if request.contains(r#""type":"shutdown""#) {
                writeln!(&conn, r#"{{"type":"done"}}"#).unwrap();
                std::fs::remove_file(&socket).unwrap();
                return;
            }
            let refusal = "this is crystal 9.9.9, but the daemon is crystal 0.0.1: \
                           run `crystal restart-server` to restart the daemon on this crystal";
            let refusal = serde_json::json!({"type": "error", "message": refusal});
            writeln!(&conn, "{refusal}").unwrap();
        }
    });

    assert_eq!(
        crystal.ok(&["restart-server"]),
        "restarted the daemon; its sessions started again, \
         since the daemon was a crystal from before handovers\n"
    );
    older.join().unwrap();
    assert_eq!(crystal.ok(&["ls"]), "");
}

#[test]
fn a_session_and_a_shell_split_off_take_the_variables_env_gives() {
    let crystal = Crystal::new();
    let script = r#"echo "said $GREETING $EMPTY."; sleep 30"#;
    let env = ["--env", "GREETING=hello=there", "-e", "EMPTY="];
    let name = crystal.ok(&[
        &["new", "-d", "-n", "greeter"][..],
        &env,
        &["sh", "-c", script],
    ]
    .concat());
    assert_eq!(name, "greeter\n");
    shows_on_screen(&crystal, "greeter", "said hello=there .");
    let said = crystal.fails(&["new", "-d", "--env", "GREETING", "true"]);
    assert!(said.contains("KEY=VALUE"), "{said}");

    // With no session to show, a split is a new shell's, which says its
    // name.
    let split = crystal.ok(&[
        "pane",
        "split",
        "--beside",
        "greeter",
        "--env",
        "PLACE=split",
    ]);
    let split = split.trim();
    let layout = crystal.ok(&["layout"]);
    assert!(
        layout.contains(&format!("the selection's: greeter\n      {split}\n")),
        "{layout}"
    );
    crystal.ok(&["send", split, "echo at $PLACE"]);
    shows_on_screen(&crystal, split, "at split");
    let said = crystal.fails(&["pane", "split", "greeter", "--env", "A=b"]);
    assert!(said.contains("cannot be used with"), "{said}");
}

#[test]
fn a_new_terminal_runs_the_shell_the_settings_name() {
    let crystal = Crystal::new();
    crystal.configure(
        "notify = false\n\n[plugins]\nmemory = false\n\n\
         [terminal]\ndefault_shell = \"/bin/sh\"\nshell_mode = \"login\"\n",
    );
    crystal.ok(&["new", "-d", "-n", "login"]);
    let sessions: serde_json::Value = serde_json::from_str(&crystal.ok(&["ls", "--json"])).unwrap();
    assert_eq!(sessions[0]["command"], serde_json::json!(["/bin/sh", "-l"]));
}

#[test]
fn the_tui_titles_its_terminal_after_the_selection_until_told_otherwise() {
    let crystal = Crystal::new();
    sessions_saying_here(&crystal, &["alpha"]);
    let said = crystal.fails(&["title", "set", "nowhere"]);
    assert!(said.contains("no TUI is running to show it"), "{said}");

    let tui = crystal.tui();
    // The terminal's own title is kept, for the end.
    tui.wrote("\x1b[22;0t", 1);
    tui.wrote("\x1b]2;crystal · alpha\x07", 1);
    crystal.ok(&["title", "set", "deploying", "now"]);
    tui.wrote("\x1b]2;deploying now\x07", 1);
    crystal.ok(&["title", "clear"]);
    tui.wrote("\x1b]2;crystal · alpha\x07", 2);
}

#[test]
fn the_tab_bar_goes_over_the_footer_with_what_the_settings_put_at_its_right() {
    let crystal = Crystal::new();
    crystal.configure(
        "notify = false\n\n[plugins]\nmemory = false\n\n[tab_bar]\nposition = \"bottom\"\n\
         right = [{ type = \"text\", text = \"prod\" }, \
         { type = \"command\", command = \"echo; echo $((1 + 1)) up\", every = \"1s\" }]\n",
    );
    sessions_saying_here(&crystal, &["alpha"]);
    let tui = crystal.tui();
    tui.shows("1 session · prod · 2 up");
    let rows = tui.rows();
    assert!(rows[22].starts_with(" crystal   1 "), "{rows:?}");
    assert!(
        rows[22].trim_end().ends_with("1 session · prod · 2 up"),
        "{rows:?}"
    );
    assert!(!rows[0].contains("crystal"), "{rows:?}");
}

#[test]
fn the_tab_bar_is_left_out_while_there_is_one_tab_when_told() {
    let crystal = Crystal::new();
    crystal.configure(
        "notify = false\n\n[plugins]\nmemory = false\n\n[tab_bar]\nhide_when_single = true\n",
    );
    sessions_saying_here(&crystal, &["alpha"]);
    let mut tui = crystal.tui();
    sidebar_shows(&tui, "alpha");
    tui.shows("alpha is here");
    assert!(!tui.text().contains(" crystal "), "{}", tui.text());
    tui.type_keys("t");
    tui.shows(" crystal   1  2 ");
}

#[test]
fn the_theme_follows_the_systems_appearance_while_the_tui_runs() {
    let crystal = Crystal::new();
    crystal.configure(
        "notify = false\n\n[plugins]\nmemory = false\n\n[appearance]\nauto_switch = true\n",
    );
    // The system, as a Mac's defaults and a Linux desktop's settings say it,
    // is dark until the file says light.
    let bin = crystal.dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let appearance = crystal.dir.path().join("appearance");
    std::fs::write(&appearance, "dark").unwrap();
    let file = appearance.display();
    script(
        &bin.join("defaults"),
        &format!(
            "if [ \"$(cat {file})\" = dark ]; then echo Dark; \
             else echo 'The domain/default pair does not exist' >&2; exit 1; fi\n"
        ),
    );
    script(&bin.join("dbus-send"), "exit 1\n");
    script(
        &bin.join("gsettings"),
        &format!(
            "if [ \"$(cat {file})\" = dark ]; then echo \"'prefer-dark'\"; else echo \"'default'\"; fi\n"
        ),
    );
    let path = path_of(&[&bin]);
    let env = [
        ("PATH", path.as_str()),
        ("SSH_CONNECTION", ""),
        ("SSH_TTY", ""),
    ];
    let tui = crystal.attach_with_env(&[], &env);
    tui.shows("No sessions yet");
    // How light the background behind crystal's name is.
    let brightness = || {
        let parser = tui.screen.lock().unwrap();
        match parser.screen().cell(0, 1).map(|cell| cell.bgcolor()) {
            Some(vt100::Color::Rgb(r, g, b)) => u32::from(r) + u32::from(g) + u32::from(b),
            other => panic!("the background is {other:?}"),
        }
    };
    assert!(brightness() < 200, "the dark theme");
    std::fs::write(&appearance, "light").unwrap();
    eventually("the theme turns light", || brightness() > 600);
}

#[test]
fn layout_commands_with_no_tui_open_lay_out_the_tabs_it_opens_with() {
    let crystal = Crystal::new();
    // With no daemon running even, one starts, and there's the one tab.
    assert_eq!(
        crystal.ok(&["layout"]),
        "1 (in front)\n  sessions  none\n  panes\n    the selection's: nothing yet\n"
    );
    sessions_saying_here(&crystal, &["alpha", "beta"]);

    // A tab closed with --kill has its sessions killed.
    assert_eq!(crystal.ok(&["tab", "new", "scratch"]), "2\n");
    sessions_saying_here(&crystal, &["delta"]);
    crystal.ok(&["tab", "close", "scratch", "--kill"]);
    assert!(crystal.row("delta").is_none());

    sessions_saying_here(&crystal, &["gamma"]);
    crystal.ok(&["pane", "split", "beta", "--beside", "alpha"]);
    assert_eq!(crystal.ok(&["tab", "new", "review"]), "2\n");
    crystal.ok(&["tab", "move", "gamma", "review"]);
    crystal.ok(&["tab", "reorder", "review", "1"]);
    let layout = crystal.ok(&["layout"]);
    assert!(
        layout.starts_with("1 review (in front)\n  sessions  gamma\n"),
        "{layout}"
    );
    assert!(
        layout.contains("\n2\n  sessions  alpha, beta\n"),
        "{layout}"
    );
    assert!(
        layout.contains("side by side, 50% first\n      the selection's: alpha\n      beta\n"),
        "{layout}"
    );
    let said = crystal.fails(&["tab", "close", "1"]);
    assert!(said.contains("--kill closes it"), "{said}");
    let said = crystal.fails(&["tab", "reorder", "review", "3"]);
    assert!(said.contains("there's no place 3"), "{said}");

    // The TUI opens on the tabs as the commands left them, and takes the
    // commands from then on.
    let tui = crystal.tui();
    tui.shows(" 1 review ");
    sidebar_shows(&tui, "gamma");
    tui.shows("gamma is here");
    crystal.ok(&["tab", "select", "2"]);
    sidebar_shows(&tui, "alpha");
    tui.shows("beta is here");
}

#[test]
fn a_layout_file_starts_what_isnt_there_and_lays_the_tabs_out() {
    let crystal = Crystal::new();
    sessions_saying_here(&crystal, &["alpha"]);
    let here =
        |name: &str| serde_json::json!(["sh", "-c", format!("echo {name} is here; sleep 30")]);
    let file = serde_json::json!({"tabs": [
        {
            "name": "dev",
            "current": true,
            "panes": {"kind": "split", "way": "right", "ratio": 0.6,
                "first": {"kind": "pane", "session": "alpha"},
                "second": {"kind": "pane", "session": "beta", "env": {"WHO": "beta"},
                    "command": ["sh", "-c", "echo $WHO is here; sleep 30"]}},
            "sessions": [{"session": "gamma", "command": here("gamma")}]
        },
        {"name": "logs", "panes": {"kind": "pane", "session": "delta", "command": here("delta")},
            "sessions": ["gone"]}
    ]});
    std::fs::write(crystal.dir.path().join("dev.json"), file.to_string()).unwrap();

    // With no TUI open, the daemon lays the tabs out, once what isn't
    // there has started.
    let out = crystal.run(&["layout", "apply", "dev.json"]);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "beta\ngamma\ndelta\n");
    assert!(said.contains("left out gone: it isn't there"), "{said}");
    shows_on_screen(&crystal, "beta", "beta is here");
    shows_on_screen(&crystal, "delta", "delta is here");
    let layout = crystal.ok(&["layout"]);
    assert!(layout.starts_with("1\n  sessions  none\n"), "{layout}");
    assert!(layout.contains("\n2 dev (in front)\n"), "{layout}");
    assert!(
        layout.contains("side by side, 60% first\n      the selection's: alpha\n      beta\n"),
        "{layout}"
    );
    assert!(layout.contains("\n3 logs\n  sessions  delta\n"), "{layout}");

    // A pane swaps with the one beside it, and takes a share of the room.
    crystal.ok(&["pane", "swap", "right", "-n", "alpha"]);
    crystal.ok(&["pane", "ratio", "0.25", "alpha"]);
    let layout = crystal.ok(&["layout"]);
    assert!(
        layout.contains("side by side, 75% first\n      beta\n      the selection's: alpha\n"),
        "{layout}"
    );
    let said = crystal.fails(&["pane", "swap", "gamma", "-n", "beta"]);
    assert!(said.contains("gamma isn't on screen"), "{said}");
    let said = crystal.fails(&["pane", "ratio", "0.5", "delta"]);
    assert!(said.contains("delta's pane isn't in a split"), "{said}");

    // An export says what starts each session again, and applied in place
    // of every tab, starts nothing that's there.
    let export = crystal.ok(&["layout", "export", "--tab", "dev"]);
    let json: serde_json::Value = serde_json::from_str(&export).unwrap();
    let dev = &json["tabs"][0];
    assert_eq!(dev["name"], "dev");
    assert_eq!(dev["panes"]["first"]["session"], "beta");
    assert_eq!(
        dev["panes"]["first"]["command"],
        serde_json::json!(["sh", "-c", "echo $WHO is here; sleep 30"])
    );
    assert_eq!(dev["sessions"][0]["command"], here("gamma"));
    let mut apply = crystal
        .command(&["layout", "apply", "--replace"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    apply
        .stdin
        .take()
        .unwrap()
        .write_all(export.as_bytes())
        .unwrap();
    let out = apply.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    let layout = crystal.ok(&["layout"]);
    assert!(layout.starts_with("1 dev (in front)\n"), "{layout}");
    assert!(!layout.contains("\n2"), "{layout}");
    assert!(layout.contains("delta"), "{layout}");

    // With the TUI open, it's the TUI that lays them out.
    let tui = crystal.tui();
    tui.shows("beta is here");
    let file = serde_json::json!({"tabs": [{"name": "more", "current": true,
        "panes": {"kind": "pane", "session": "epsilon", "command": here("epsilon")}}]});
    std::fs::write(crystal.dir.path().join("more.json"), file.to_string()).unwrap();
    assert_eq!(crystal.ok(&["layout", "apply", "more.json"]), "epsilon\n");
    tui.shows(" 2 more ");
    tui.shows("epsilon is here");
    let said = crystal.fails(&["layout", "apply", "missing.json"]);
    assert!(said.contains("couldn't read missing.json"), "{said}");
}

#[test]
fn a_split_and_a_tab_from_the_command_line_reach_the_tui() {
    let crystal = Crystal::new();
    sessions_saying_here(&crystal, &["alpha", "beta"]);
    let tui = crystal.tui();
    tui.shows("alpha is here");

    crystal.ok(&["pane", "split", "beta", "--beside", "alpha"]);
    tui.shows("beta is here");
    tui.shows("alpha is here");
    let layout = crystal.ok(&["layout"]);
    assert!(
        layout.contains("side by side, 50% first\n      the selection's: alpha\n      beta\n"),
        "{layout}"
    );
    let json: serde_json::Value = serde_json::from_str(&crystal.ok(&["layout", "--json"])).unwrap();
    assert_eq!(json["tabs"][0]["panes"]["second"]["session"], "beta");
    // Its terminal never says whether it has the focus.
    assert_eq!(json["presence"], "unknown");
    let said = crystal.fails(&["pane", "split", "beta", "--beside", "gone"]);
    assert!(said.contains("there's no session called gone"), "{said}");

    // A new tab comes to the front, and a session started then goes in it.
    assert_eq!(crystal.ok(&["tab", "new", "review"]), "2\n");
    tui.shows(" 2 review ");
    sidebar_hides(&tui, "alpha");
    sessions_saying_here(&crystal, &["gamma"]);
    sidebar_shows(&tui, "gamma");
    tui.shows("gamma is here");

    crystal.ok(&["tab", "select", "1"]);
    sidebar_shows(&tui, "alpha");
    tui.shows("beta is here");
}

#[test]
fn a_split_from_a_session_goes_beside_it() {
    let crystal = Crystal::new();
    sessions_saying_here(&crystal, &["agent", "other", "tests"]);
    let mut tui = crystal.tui();
    tui.shows("agent is here");
    tui.type_keys("j");
    tui.shows("other is here");

    // Run in agent, the split goes beside it, and it's selected again to
    // show there.
    let sessions: serde_json::Value = serde_json::from_str(&crystal.ok(&["ls", "--json"])).unwrap();
    let agent = sessions
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["name"] == "agent");
    let id = agent.unwrap()["id"].as_str().unwrap();
    let out = crystal
        .command(&["pane", "split", "tests", "--down"])
        .env("CRYSTAL_SESSION_ID", id)
        .env("CRYSTAL_SOCKET", &crystal.socket)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    tui.shows("tests is here");
    tui.shows("agent is here");
    tui.hides("other is here");
}

#[test]
fn layout_commands_go_to_the_tui_used_last() {
    let crystal = Crystal::new();
    sessions_saying_here(&crystal, &["alpha"]);
    let mut first = crystal.tui();
    first.shows("alpha is here");
    let second = crystal.tui();
    second.shows("alpha is here");

    // The second opened last.
    crystal.ok(&["tab", "rename", "1", "second"]);
    second.shows(" 1 second ");

    // A key in the first makes it the one used last.
    first.type_keys("?");
    first.shows("In the sidebar");
    first.type_keys("?");
    first.hides("In the sidebar");
    crystal.ok(&["tab", "rename", "1", "first"]);
    first.shows(" 1 first ");
    second.shows(" 1 second ");
}

#[test]
fn layout_commands_reach_the_tui_after_a_handover() {
    let crystal = Crystal::new();
    sessions_saying_here(&crystal, &["alpha", "beta"]);
    let tui = crystal.tui();
    tui.shows("alpha is here");

    assert_eq!(
        crystal.ok(&["restart-server"]),
        "restarted the daemon, and its sessions carried on\n"
    );
    // The TUI offers again to the daemon that took over, straight away.
    crystal.ok(&["pane", "split", "beta", "--beside", "alpha"]);
    tui.shows("beta is here");
    tui.shows("alpha is here");
}

#[test]
fn restart_server_cold_starts_the_running_sessions_again() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "keeper", "sleep", "300"]);
    eventually("the session is saved", || {
        crystal.saved().contains("keeper")
    });
    let old_pid = crystal.pid("keeper");

    assert_eq!(
        crystal.ok(&["restart-server", "--cold"]),
        "restarted the daemon\n"
    );
    assert_eq!(crystal.row("keeper").unwrap()[1], "running");
    assert_ne!(crystal.pid("keeper"), old_pid);
    eventually("the old program has gone with its daemon", || {
        !alive(old_pid)
    });
}

#[test]
fn the_default_socket_keeps_its_state_however_its_daemon_is_started() {
    // A runtime dir of the test's own, and a link to it: its default socket
    // spelled two ways, whose state goes in a state dir of the test's own.
    let crystal = Crystal::new();
    let run = crystal.dir.path().join("run");
    std::fs::create_dir(&run).unwrap();
    let link = crystal.dir.path().join("link");
    std::os::unix::fs::symlink(&run, &link).unwrap();
    let state = crystal.dir.path().join("state");
    let at = |socket: &Path, args: &[&str]| {
        let out = outside_crystal(CRYSTAL)
            .arg("--socket")
            .arg(socket)
            .args(args)
            .current_dir(crystal.dir.path())
            .env("XDG_RUNTIME_DIR", &run)
            .env("XDG_STATE_HOME", &state)
            .env("XDG_CONFIG_HOME", crystal.config_home())
            // Nor the user's Claude Code config, where a daemon may update
            // the skill as it starts.
            .env(
                "CLAUDE_CONFIG_DIR",
                crystal.dir.path().join("claude-config"),
            )
            .envs(PLAIN_GIT)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "crystal {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let socket = run.join("crystal/default.sock");
    at(&socket, &["new", "-n", "keeper", "sleep", "300"]);
    let db = state.join("crystal/crystal.db");
    eventually("the session is saved in the state dir", || {
        rusqlite::Connection::open(&db)
            .and_then(|conn| {
                conn.query_row("SELECT count(*) FROM sessions", [], |row| {
                    row.get::<_, i64>(0)
                })
            })
            .is_ok_and(|count| count == 1)
    });

    // Started again through the link: the same socket, so the same state,
    // and the session comes back.
    let through_link = link.join("crystal/default.sock");
    assert_eq!(
        at(&through_link, &["restart-server", "--cold"]),
        "restarted the daemon\n"
    );
    let listed = at(&socket, &["ls"]);
    at(&socket, &["kill-server"]);
    assert!(listed.contains("keeper"), "{listed}");
    assert!(!run.join("crystal/default.db").exists());
    assert!(!link.join("crystal/default.db").exists());
}

/// Servers named with `--server`, in a runtime dir and a state dir of the
/// test's own. crystal runs without `--socket`, and without the
/// `CRYSTAL_SOCKET` or `CRYSTAL_SERVER` of a session the tests may be run
/// in, which would reach the user's own daemon.
struct Servers {
    crystal: Crystal,
}

impl Servers {
    fn new() -> Servers {
        Servers {
            crystal: Crystal::new(),
        }
    }

    fn dir(&self) -> &Path {
        self.crystal.dir.path()
    }

    /// The test's `XDG_RUNTIME_DIR`: the sockets are in its `crystal`.
    fn run_dir(&self) -> PathBuf {
        self.dir().join("run")
    }

    /// Where each server but the default keeps its state.
    fn kept(&self) -> PathBuf {
        self.dir().join("state/crystal/servers")
    }

    fn command(&self, args: &[&str]) -> Command {
        self.command_of(Path::new(CRYSTAL), args)
    }

    /// Like [`Servers::command`], run by the crystal at `program`.
    fn command_of(&self, program: &Path, args: &[&str]) -> Command {
        let mut command = outside_crystal(program);
        command
            .args(args)
            .current_dir(self.dir())
            .env("XDG_RUNTIME_DIR", self.run_dir())
            .env("XDG_STATE_HOME", self.dir().join("state"))
            .env("XDG_CONFIG_HOME", self.crystal.config_home())
            .env("CLAUDE_CONFIG_DIR", self.crystal.claude_config_dir())
            .envs(PLAIN_GIT)
            .envs(QUIET);
        command
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.command(args).output().unwrap();
        assert!(
            out.status.success(),
            "crystal {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn fails(&self, args: &[&str]) -> String {
        let out = self.command(args).output().unwrap();
        assert!(!out.status.success(), "crystal {args:?} succeeded");
        String::from_utf8(out.stderr).unwrap()
    }

    /// The names of the sessions `ls` lists, run with `args` before it.
    fn sessions(&self, args: &[&str]) -> Vec<String> {
        let ls = [args, &["ls"]].concat();
        let listed = self.ok(&ls);
        let names = listed
            .lines()
            .skip(1)
            .filter_map(|line| line.split(' ').next());
        names.map(String::from).collect()
    }
}

impl Drop for Servers {
    /// Stops every server the test started.
    fn drop(&mut self) {
        let sockets = std::fs::read_dir(self.run_dir().join("crystal"));
        for entry in sockets.into_iter().flatten().flatten() {
            let socket = entry.path();
            if socket
                .extension()
                .is_some_and(|extension| extension == "sock")
            {
                let _ = self
                    .command(&["kill-server"])
                    .arg("--socket")
                    .arg(&socket)
                    .output();
            }
        }
    }
}

#[test]
fn two_named_servers_run_side_by_side_each_with_its_own_sessions_and_state() {
    let servers = Servers::new();
    let new = |server: &str, name: &str| {
        servers.ok(&["--server", server, "new", "-d", "-n", name, "sleep", "30"])
    };
    assert_eq!(new("work", "one"), "one\n");
    assert_eq!(new("side", "two"), "two\n");

    assert_eq!(servers.sessions(&["--server", "work"]), ["one"]);
    assert_eq!(servers.sessions(&["-L", "side"]), ["two"]);
    let ls = servers
        .command(&["ls"])
        .env("CRYSTAL_SERVER", "side")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&ls.stdout).contains("two"));
    // The default server is left alone, and isn't started.
    assert!(servers.sessions(&[]).is_empty());
    assert!(!servers.run_dir().join("crystal/default.sock").exists());

    // Each keeps its state in a directory of its own in the state dir,
    // which a reboot doesn't empty, rather than beside its socket.
    assert!(servers.kept().join("work/crystal.db").exists());
    assert!(servers.kept().join("side/crystal.db").exists());
    assert!(!servers.run_dir().join("crystal/work.db").exists());

    assert_eq!(
        servers.ok(&["server"]),
        "NAME     STATE    SESSIONS\n\
         default  stopped  0\n\
         side     running  1\n\
         work     running  1\n"
    );
    let listed: serde_json::Value =
        serde_json::from_str(&servers.ok(&["servers", "--json"])).unwrap();
    let work = &listed[2];
    assert_eq!(work["name"], "work");
    assert_eq!(work["running"], true);
    assert_eq!(work["sessions"], 1);
    let state = servers.kept().join("work");
    assert_eq!(work["state"], state.to_str().unwrap());
}

#[test]
fn restart_server_on_a_named_server_hands_over_that_server_alone() {
    let servers = Servers::new();
    for (server, name) in [("work", "one"), ("side", "two")] {
        servers.ok(&["--server", server, "new", "-d", "-n", name, "sleep", "30"]);
    }
    let pid = |server: &str, name: &str| {
        let listed = servers.ok(&["--server", server, "ls", "--json"]);
        let listed: Vec<serde_json::Value> = serde_json::from_str(&listed).unwrap();
        let found = listed.iter().find(|session| session["name"] == name);
        found.unwrap()["pid"].as_u64().unwrap()
    };
    let (one, two) = (pid("work", "one"), pid("side", "two"));
    let side_log = servers.run_dir().join("crystal/side.log");
    let side_before = std::fs::read_to_string(&side_log).unwrap_or_default();

    assert_eq!(
        servers.ok(&["--server", "work", "restart-server"]),
        "restarted the daemon, and its sessions carried on\n"
    );
    assert_eq!(pid("work", "one"), one);
    assert_eq!(pid("side", "two"), two);
    // Its state is where it was; the other server was never asked.
    assert!(servers.kept().join("work/crystal.db").exists());
    let handed = servers.ok(&["--server", "work", "events"]);
    assert!(handed.contains("daemon.handed_over"), "{handed}");
    let side = servers.ok(&["--server", "side", "events"]);
    assert!(!side.contains("daemon.handed_over"), "{side}");
    assert_eq!(
        std::fs::read_to_string(&side_log).unwrap_or_default(),
        side_before
    );
}

#[test]
fn a_session_in_a_named_server_reaches_its_own_server() {
    let servers = Servers::new();
    servers.ok(&[
        "--server", "work", "new", "-d", "-n", "other", "sleep", "30",
    ]);
    let script = format!("echo $CRYSTAL_SERVER > server; '{CRYSTAL}' ls > seen; sleep 30");
    let new = [
        "--server", "side", "new", "-d", "-n", "inner", "sh", "-c", &script,
    ];
    servers.ok(&new);
    assert_eq!(written(&servers.dir().join("server")), "side\n");
    let seen = written(&servers.dir().join("seen"));
    assert!(seen.contains("inner"), "{seen}");
    assert!(!seen.contains("other"), "{seen}");
}

#[test]
fn a_server_stops_by_name_and_its_state_is_deleted_once_it_has() {
    let servers = Servers::new();
    for (server, name) in [("work", "one"), ("side", "two")] {
        servers.ok(&["--server", server, "new", "-d", "-n", name, "sleep", "30"]);
    }
    let refused = servers.fails(&["server", "delete", "work"]);
    assert!(refused.contains("the server work is running"), "{refused}");

    servers.ok(&["server", "stop", "work"]);
    assert!(!servers.run_dir().join("crystal/work.sock").exists());
    assert_eq!(servers.sessions(&["--server", "side"]), ["two"]);
    assert_eq!(
        servers.ok(&["server"]),
        "NAME     STATE    SESSIONS\n\
         default  stopped  0\n\
         side     running  1\n\
         work     stopped  0\n"
    );
    let again = servers.fails(&["server", "stop", "work"]);
    assert!(again.contains("the server work isn't running"), "{again}");

    servers.ok(&["server", "delete", "work"]);
    assert!(!servers.kept().join("work").exists());
    assert!(!servers.run_dir().join("crystal/work.log").exists());
    assert!(servers.kept().join("side").exists());
    let listed = servers.ok(&["server"]);
    assert!(!listed.contains("work"), "{listed}");
    let gone = servers.fails(&["server", "rm", "work"]);
    assert!(gone.contains("there's no server called work"), "{gone}");
    let default = servers.fails(&["server", "delete", "default"]);
    assert!(
        default.contains("the default server can't be deleted"),
        "{default}"
    );
    let bad = servers.fails(&["--server", "../work", "ls"]);
    assert!(bad.contains("can't name a server"), "{bad}");
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
    let settings: serde_json::Value = claude_settings(&args);
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
        .command(&["restart-server", "--cold"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());

    // Back in its conversation, which has had the task already.
    let args = written(&crystal.dir.path().join("args"));
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(args[4..6], ["--resume", "abc-123"]);
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
        self.start_daemon_with(&[("CRYSTAL_PRETEND_VERSION", "0.0.1")])
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

    // Restarting goes through whatever the version, and ends the mismatch:
    // the daemon is handed over to this crystal, in the same process.
    assert_eq!(crystal.ok(&["restart-server"]), "restarted the daemon\n");
    assert_eq!(crystal.ok(&["ls"]), "");
    assert!(older.try_wait().unwrap().is_none());
    crystal.ok(&["kill-server"]);
    older.wait().unwrap();
}

#[test]
fn a_crystal_older_than_the_daemon_is_told_to_start_again() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "keeper", "sleep", "300"]);
    let out = crystal
        .command(&["ls"])
        .env("CRYSTAL_PRETEND_VERSION", "0.0.1")
        .output()
        .unwrap();
    let ours = env!("CARGO_PKG_VERSION");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "crystal: this is crystal 0.0.1, but the daemon is crystal {ours}, which is newer: \
             quit and run crystal again to use it\n"
        )
    );
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
    // The file is written as the last line goes out, before the daemon may
    // have read it off the terminal.
    shows_on_screen(&crystal, "printer", "line 60");

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
fn the_history_keeps_more_than_tmuxs_two_thousand_rows() {
    let crystal = Crystal::new();
    let printing = "for i in $(seq 1 3000); do echo line $i; done; echo > printed; sleep 30";
    crystal.ok(&["new", "-n", "printer", "sh", "-c", printing]);
    written(&crystal.dir.path().join("printed"));

    // The file is written as the last line goes out, before the daemon has
    // read it all off the terminal.
    let read = || crystal.ok(&["read", "printer", "--history"]);
    eventually("the last line is in", || read().contains("line 3000\n"));
    let all = read();
    let lines: Vec<&str> = all.lines().collect();
    assert_eq!(lines[0], "line 1");
    assert_eq!(lines[2999], "line 3000");
}

#[test]
fn scrollback_lines_in_the_config_is_how_much_history_a_session_keeps() {
    let crystal = Crystal::new();
    crystal.configure(
        "notify = false\nname_from_prompt = false\nscrollback_lines = 10\n\n[plugins]\nmemory = false\n",
    );
    crystal.ok(&["new", "-n", "printer", "sh", "-c", LONG_OUTPUT]);
    written(&crystal.dir.path().join("printed"));
    shows_on_screen(&crystal, "printer", "line 60");

    // 40 rows on the screen of a session nobody has looked at, the last of
    // them the empty one under the cursor, and 10 above it.
    let all = crystal.ok(&["read", "printer", "--history"]);
    let lines: Vec<&str> = all.lines().collect();
    assert_eq!(lines.len(), 49, "{all}");
    assert_eq!(lines[0], "line 12");
    assert_eq!(lines[48], "line 60");
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
    shows_on_screen(&crystal, "inline", "out 30");

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
        self.notices_to_file_with("")
    }

    /// The same, with `settings` after it in the config.
    fn notices_to_file_with(&self, settings: &str) -> PathBuf {
        let line = "$CRYSTAL_NOTICE_SESSION $CRYSTAL_NOTICE_ACTIVITY: $CRYSTAL_NOTICE";
        self.notices_like(line, settings)
    }

    /// Has notices go to a file as `line` says them, with `settings` after
    /// it in the config, and returns the file.
    fn notices_like(&self, line: &str, settings: &str) -> PathBuf {
        let notices = self.dir.path().join("notices");
        self.configure(&format!(
            "notify_command = '''echo \"{line}\" >> {}'''\n{settings}",
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

#[test]
fn the_user_is_told_only_once_a_session_has_needed_them_for_a_while() {
    let crystal = Crystal::new();
    let notices = crystal.notices_to_file_with("\n[notifications]\nafter_secs = 2\n");
    crystal.new_pretend_agent("agent", ASKING_AGENT);

    std::fs::write(crystal.dir.path().join("ask"), "").unwrap();
    eventually("the session is waiting", || {
        crystal.row("agent").unwrap()[1] == "waiting"
    });
    eventually("the user is told it's waiting", || {
        lines_in(&notices) == ["agent waiting: agent is waiting on you"]
    });
    let after = told_after(&crystal, &notices, "session.waiting");
    assert!(after >= Duration::from_millis(1500), "told {after:?} after");

    // Done straight after, it's told of only once it has stayed done.
    std::fs::write(crystal.dir.path().join("rest"), "").unwrap();
    eventually("the session is done", || {
        crystal.row("agent").unwrap()[1] == "done"
    });
    eventually("the user is told it's done", || {
        lines_in(&notices).len() == 2
    });
    let after = told_after(&crystal, &notices, "session.done");
    assert!(after >= Duration::from_millis(1500), "told {after:?} after");
}

/// How long after the daemon logged the latest `kind` of the session called
/// `agent` the last notice was written to `notices`: both by the clock, not
/// by when this test, which can be slow to look, saw them.
fn told_after(crystal: &Crystal, notices: &Path, kind: &str) -> Duration {
    let logged = events(crystal, &["-n", "agent", "-k", kind]);
    let happened = logged.last().unwrap()["at"].as_u64().unwrap();
    let written = std::fs::metadata(notices).unwrap().modified().unwrap();
    let written = written.duration_since(std::time::UNIX_EPOCH).unwrap();
    written.saturating_sub(Duration::from_millis(happened))
}

#[test]
fn a_question_answered_before_its_time_is_never_told_of() {
    let crystal = Crystal::new();
    let notices = crystal.notices_to_file_with("\n[notifications]\nafter_secs = 3\n");
    crystal.new_pretend_agent("agent", ASKING_AGENT);

    std::fs::write(crystal.dir.path().join("ask"), "").unwrap();
    eventually("the session is waiting", || {
        crystal.row("agent").unwrap()[1] == "waiting"
    });
    std::fs::write(crystal.dir.path().join("rest"), "").unwrap();
    eventually("the user is told it's done", || {
        !lines_in(&notices).is_empty()
    });
    assert_eq!(lines_in(&notices), ["agent done: agent is done"]);
}

/// Where the daemon has the user, by what the TUIs' terminals say of their
/// focus: `here`, `away` or `unknown`.
fn presence(crystal: &Crystal) -> String {
    let layout = crystal.ok(&["layout", "--json"]);
    let layout: serde_json::Value = serde_json::from_str(&layout).unwrap();
    layout["presence"].as_str().unwrap().to_string()
}

#[test]
fn a_session_shown_in_a_terminal_without_the_focus_isn_t_watched() {
    let crystal = Crystal::new();
    let notices = crystal.notices_to_file();
    crystal.new_pretend_agent("agent", ASKING_AGENT);
    let mut tui = crystal.tui();
    // Its pane shows it.
    tui.shows("▸ agent ─");

    // The terminal says it has lost the focus.
    tui.type_keys("\x1b[O");
    eventually("the daemon hears it", || presence(&crystal) == "away");
    std::fs::write(crystal.dir.path().join("ask"), "").unwrap();
    eventually("the user is told it's waiting", || {
        lines_in(&notices) == ["agent waiting: agent is waiting on you"]
    });
}

#[test]
fn unfocused_only_keeps_quiet_while_crystal_has_the_focus() {
    let crystal = Crystal::new();
    let notices = crystal.notices_to_file_with("\n[notifications]\nunfocused_only = true\n");
    crystal.new_pretend_agent("agent", ASKING_AGENT);
    crystal.ok(&["new", "-n", "other", "sleep", "30"]);
    let mut tui = crystal.tui();
    tui.shows("other");
    // The one in the pane is watched: the agent mustn't be.
    crystal.ok(&["pane", "focus", "other"]);
    tui.shows("❯ other");
    tui.type_keys("\x1b[I");
    eventually("the daemon hears it", || presence(&crystal) == "here");

    std::fs::write(crystal.dir.path().join("ask"), "").unwrap();
    eventually("the session is waiting", || {
        crystal.row("agent").unwrap()[1] == "waiting"
    });
    thread::sleep(Duration::from_millis(800));
    assert!(lines_in(&notices).is_empty(), "{:?}", lines_in(&notices));

    tui.type_keys("\x1b[O");
    eventually("the daemon hears it", || presence(&crystal) == "away");
    std::fs::write(crystal.dir.path().join("rest"), "").unwrap();
    eventually("the user is told it's done", || {
        lines_in(&notices) == ["agent done: agent is done"]
    });
}

#[test]
fn crystal_notify_tells_the_user_and_its_click_goes_to_the_session() {
    let crystal = Crystal::new();
    let notices = crystal.notices_like("$CRYSTAL_NOTICE|$CRYSTAL_NOTICE_JUMP", "");
    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);
    crystal.ok(&["new", "-n", "other", "sleep", "30"]);

    crystal.ok(&["notify", "the", "build", "is", "green"]);
    eventually("the user is told", || {
        lines_in(&notices) == ["the build is green|"]
    });
    let err = crystal.fails(&["notify", "-n", "nobody", "hi"]);
    assert!(err.contains("no session named nobody"), "{err}");

    let tui = crystal.tui();
    tui.shows("other");
    crystal.ok(&["pane", "focus", "other"]);
    tui.shows("❯ other");
    crystal.ok(&["notify", "-n", "agent", "tests", "pass"]);
    eventually("the user is told", || lines_in(&notices).len() == 2);
    let line = lines_in(&notices).pop().unwrap();
    let (text, jump) = line.split_once('|').unwrap();
    assert_eq!(text, "agent: tests pass");
    assert!(jump.ends_with("pane focus --raise -- agent"), "{jump}");

    // A click runs the jump.
    let clicked = outside_crystal("sh").arg("-c").arg(jump).output().unwrap();
    assert!(
        clicked.status.success(),
        "{}",
        String::from_utf8_lossy(&clicked.stderr)
    );
    tui.shows("❯ agent");
}

/// What a terminal sends for a click at `(column, row)` on its screen,
/// counted from 0, the SGR way: a press, then a release.
fn click(column: usize, row: usize) -> String {
    let (x, y) = (column + 1, row + 1);
    format!("\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m")
}

/// What a terminal sends for a click at `(column, row)` with Ctrl held.
fn ctrl_click(column: usize, row: usize) -> String {
    let (x, y) = (column + 1, row + 1);
    format!("\x1b[<16;{x};{y}M\x1b[<16;{x};{y}m")
}

/// What a terminal sends for the mouse moving to `(column, row)`, no
/// button down, with Ctrl held or not.
fn mouse_move(column: usize, row: usize, ctrl: bool) -> String {
    let button = if ctrl { 51 } else { 35 };
    format!("\x1b[<{button};{};{}M", column + 1, row + 1)
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
fn the_wheel_over_a_pager_sends_it_the_arrow_keys() {
    let crystal = Crystal::new();
    // Goes to the alternate screen, as less does, without asking for the
    // mouse, then writes down the first keys it hears: three arrows, three
    // bytes each.
    let script = r"stty raw -echo; printf '\033[?1049hpaging'; echo > listening;
        dd bs=1 count=9 2>/dev/null > keys; echo >> keys; sleep 30";
    crystal.ok(&["new", "-n", "pager", "sh", "-c", script]);
    written(&crystal.dir.path().join("listening"));

    let mut tui = crystal.tui();
    tui.shows("paging");
    // Over the pane, the keyboard still in the sidebar.
    tui.type_keys(&wheel_up(50, 10));
    let keys = written(&crystal.dir.path().join("keys"));
    assert_eq!(keys, "\x1b[A\x1b[A\x1b[A\n");
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
    let settings: serde_json::Value = claude_settings(&args);
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
    assert_eq!(
        args[4..7],
        ["--resume", "abc-123", "--append-system-prompt"]
    );
    assert!(!args.contains(&"--"), "{args:?}");
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

/// Puts a `git` in `bin` that writes each command it's given down in
/// `git.log` in `dir`, and holds `worktree remove` until there's a file
/// called `go` there, then runs the real one. Gives back the log's path.
fn held_worktree_remove(bin: &Path, dir: &Path) -> PathBuf {
    let found = outside_crystal("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let git = String::from_utf8(found.stdout).unwrap();
    let (log, go) = (dir.join("git.log"), dir.join("go"));
    script(
        &bin.join("git"),
        &format!(
            "echo \"$*\" >> '{}'\n\
             case \"$*\" in *\"worktree remove\"*)\n\
             while [ ! -e '{}' ]; do sleep 0.05; done ;;\n\
             esac\n\
             exec {} \"$@\"\n",
            log.display(),
            go.display(),
            git.trim()
        ),
    );
    log
}

/// What `child` printed, once it has ended, which it must do in a while.
fn answered(mut child: std::process::Child) -> Output {
    eventually("the command has been answered", || {
        child.try_wait().unwrap().is_some()
    });
    child.wait_with_output().unwrap()
}

/// How many of the commands `held_worktree_remove`'s git wrote down in
/// `log` have `words` in them.
fn git_ran(log: &Path, words: &str) -> usize {
    let log = std::fs::read_to_string(log).unwrap_or_default();
    log.lines().filter(|line| line.contains(words)).count()
}

#[test]
fn quitting_the_tui_doesn_t_stop_the_worktree_it_asked_to_remove() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "app");
    let repo_arg = repo.to_str().unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let log = held_worktree_remove(&bin, dir);
    // The daemon removes it, with the git that holds.
    let daemon = crystal.start_daemon_with(&[("PATH", &path_with(&bin))]);
    crystal.ok(&[
        "new", "-n", "fixer", "-c", repo_arg, "-w", "fix", "sh", "-c", "exit 0",
    ]);
    eventually("fixer has ended", || {
        crystal.row("fixer").unwrap()[1] == "exited 0"
    });
    let worktree = dir.join("app.worktrees/fix");

    let mut tui = crystal.tui();
    tui.shows("■ fixer");
    tui.type_keys("W");
    tui.shows("remove worktree fix? y/n");
    tui.type_keys("y");
    tui.shows("removing…");
    eventually("git is removing it", || {
        git_ran(&log, "worktree remove") == 1
    });
    tui.type_keys("q");
    assert!(tui.exit());

    std::fs::write(dir.join("go"), "").unwrap();
    eventually("the worktree is gone", || !worktree.exists());
    eventually("its ended session went with it", || {
        crystal.row("fixer").is_none()
    });
    let removed = crystal.ok(&["events", "-k", "worktree.removed"]);
    assert_eq!(removed.lines().count(), 1, "{removed}");
    assert!(removed.contains("app.worktrees/fix on fix"), "{removed}");
    crash(daemon);
}

#[test]
fn a_tui_opened_while_a_worktree_is_being_removed_says_so() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "app");
    let repo_arg = repo.to_str().unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let log = held_worktree_remove(&bin, dir);
    let daemon = crystal.start_daemon_with(&[("PATH", &path_with(&bin))]);
    crystal.ok(&[
        "new", "-n", "fixer", "-c", repo_arg, "-w", "fix", "sh", "-c", "exit 0",
    ]);
    eventually("fixer has ended", || {
        crystal.row("fixer").unwrap()[1] == "exited 0"
    });
    let removal = crystal
        .command(&["worktree", "rm", "app.worktrees/fix"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    eventually("git is removing it", || {
        git_ran(&log, "worktree remove") == 1
    });

    // Asked for on the command line before the TUI opened, it's the
    // daemon that says so.
    let mut tui = crystal.tui();
    tui.shows("■ fixer");
    tui.shows("removing…");
    tui.type_keys("W");
    tui.shows("already removing fix");

    std::fs::write(dir.join("go"), "").unwrap();
    let out = answered(removal);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    tui.hides("removing…");
    tui.hides("■ fixer");
    crash(daemon);
}

#[test]
fn a_worktree_being_removed_as_the_daemon_hands_over_is_finished_by_the_next() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "app");
    let repo_arg = repo.to_str().unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let log = held_worktree_remove(&bin, dir);
    let daemon = crystal.start_daemon_with(&[("PATH", &path_with(&bin))]);
    crystal.ok(&[
        "new", "-n", "fixer", "-c", repo_arg, "-w", "fix", "sh", "-c", "exit 0",
    ]);
    eventually("fixer has ended", || {
        crystal.row("fixer").unwrap()[1] == "exited 0"
    });
    let worktree = dir.join("app.worktrees/fix");
    let remove = || {
        crystal
            .command(&["worktree", "rm", "app.worktrees/fix"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };

    let first = remove();
    eventually("git is removing it", || {
        git_ran(&log, "worktree remove") == 1
    });
    // Asked again meanwhile, it's the same removal: the daemon looks for
    // the worktree's repository, then waits with the first.
    let looked = git_ran(&log, "app.worktrees/fix rev-parse");
    let second = remove();
    eventually("the second has been read", || {
        git_ran(&log, "app.worktrees/fix rev-parse") > looked
    });
    assert_eq!(
        crystal.ok(&["restart-server"]),
        "restarted the daemon, and its sessions carried on\n"
    );
    assert!(worktree.is_dir(), "git is still holding");

    std::fs::write(dir.join("go"), "").unwrap();
    for removal in [first, second] {
        let out = answered(removal);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{err}");
    }
    assert!(!worktree.exists());
    assert!(crystal.row("fixer").is_none());
    // git removed it once, the new daemon finding it gone.
    assert_eq!(git_ran(&log, "worktree remove"), 1);
    let removed = crystal.ok(&["events", "-k", "worktree.removed"]);
    assert_eq!(removed.lines().count(), 1, "{removed}");
    crash(daemon);
}

#[test]
fn a_removal_git_refuses_after_a_handover_is_tried_again_and_says_why() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "app");
    let repo_arg = repo.to_str().unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let log = held_worktree_remove(&bin, dir);
    let daemon = crystal.start_daemon_with(&[("PATH", &path_with(&bin))]);
    crystal.ok(&["new", "-n", "x", "-c", repo_arg, "-w", "fix", "true"]);
    eventually("x has ended", || crystal.row("x").unwrap()[1] == "exited 0");
    let worktree = dir.join("app.worktrees/fix");
    std::fs::write(worktree.join("notes.txt"), "half done\n").unwrap();

    let removal = crystal
        .command(&["worktree", "rm", "app.worktrees/fix"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    eventually("git is removing it", || {
        git_ran(&log, "worktree remove") == 1
    });
    crystal.ok(&["restart-server"]);
    std::fs::write(dir.join("go"), "").unwrap();

    // Still there after the first git, the next daemon has git try again,
    // which says why it won't.
    let out = answered(removal);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("untracked"), "{err}");
    assert_eq!(git_ran(&log, "worktree remove"), 2);
    assert!(worktree.join("notes.txt").exists());
    crash(daemon);
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
    // Asked whether the worktree goes too, a no keeps it.
    tui.shows("nothing else is in worktree fix: remove it too? y/n");
    tui.type_keys("n");
    tui.hides("remove it too?");
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
fn killing_the_last_session_in_a_worktree_can_take_the_worktree_with_it() {
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
    tui.type_keys("jx");
    tui.shows("kill fixer? y/n");
    tui.type_keys("y");
    tui.shows("nothing else is in worktree fix: remove it too? y/n");
    tui.type_keys("y");
    eventually("the worktree is gone", || !worktree.exists());
    tui.hides("⎇ fix");
    assert!(crystal.row("planner").is_some());
}

/// The config of a test whose new worktrees go in `directory`.
fn worktrees_in(directory: &Path) -> String {
    format!(
        "notify = false\nname_from_prompt = false\n\n[plugins]\nmemory = false\n\n\
         [worktrees]\ndirectory = \"{}\"\n",
        directory.display()
    )
}

#[test]
fn worktree_create_makes_one_where_the_settings_say_and_labels_it() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let trees = dir.join("trees");
    crystal.configure(&worktrees_in(&trees));
    let repo = git_repo(dir, "app");
    let repo_arg = repo.to_str().unwrap();

    let made = crystal.ok(&[
        "worktree",
        "create",
        "spike",
        "--label",
        "try sqlite",
        "-C",
        repo_arg,
    ]);
    let spike = trees.join("app/spike").canonicalize().unwrap();
    assert_eq!(Path::new(made.trim()), spike);
    assert!(git(&repo, &["branch", "--list", "spike"]).contains("spike"));

    // With no branch, one with a made-up name, and wherever --path says.
    let elsewhere = dir.join("elsewhere");
    let made = crystal.ok(&[
        "worktree",
        "create",
        "--path",
        elsewhere.to_str().unwrap(),
        "-C",
        repo_arg,
    ]);
    assert_eq!(Path::new(made.trim()), elsewhere.canonicalize().unwrap());

    // `new -w` makes its worktree in the settings' directory too.
    crystal.ok(&[
        "new", "-d", "-n", "fixer", "-c", repo_arg, "-w", "fix", "sleep", "30",
    ]);
    assert!(trees.join("app/fix").is_dir());

    let listed: serde_json::Value =
        serde_json::from_str(&crystal.ok(&["worktree", "list", "--json", "-C", repo_arg])).unwrap();
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 4, "{listed:?}");
    assert_eq!(listed[0]["main"], true);
    assert_eq!(listed[0]["branch"], "main");
    let by_branch = |branch: &str| {
        listed
            .iter()
            .find(|worktree| worktree["branch"] == branch)
            .unwrap_or_else(|| panic!("no {branch} in {listed:?}"))
    };
    assert_eq!(by_branch("spike")["label"], "try sqlite");
    assert_eq!(by_branch("fix")["sessions"], serde_json::json!(["fixer"]));
    let table = crystal.ok(&["worktree", "list", "-C", repo_arg]);
    assert!(table.contains("⎇ spike"), "{table}");
    assert!(table.contains("try sqlite"), "{table}");

    crystal.ok(&["worktree", "label", "spike", "", "-C", repo_arg]);
    let listed: serde_json::Value =
        serde_json::from_str(&crystal.ok(&["worktree", "list", "--json", "-C", repo_arg])).unwrap();
    let spike = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|worktree| worktree["branch"] == "spike")
        .unwrap()
        .clone();
    assert_eq!(spike["label"], serde_json::Value::Null);
}

#[test]
fn worktree_open_starts_a_session_in_a_worktree_found_by_its_branch() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["worktree", "create", "spike", "-C", repo_arg]);
    let spike = crystal.dir.path().join("app.worktrees/spike");

    let name = crystal.ok(&[
        "worktree",
        "open",
        "spike",
        "-d",
        "-n",
        "opened",
        "--label",
        "the spike",
        "-C",
        repo_arg,
        "sh",
        "-c",
        "pwd > where; sleep 30",
    ]);
    assert_eq!(name.trim(), "opened");
    let started_in = written(&spike.join("where"));
    assert_eq!(
        Path::new(started_in.trim()).canonicalize().unwrap(),
        spike.canonicalize().unwrap()
    );
    assert_eq!(crystal.row("opened").unwrap()[4], "spike");
    let err = crystal.fails(&["worktree", "open", "nowhere", "-d", "-C", repo_arg]);
    assert!(err.contains("no worktree at nowhere"), "{err}");
}

/// A hook that writes down what it was run with, a line each time, in
/// `log`.
fn recording_hook(dir: &Path, log: &Path) -> PathBuf {
    let hook = dir.join("hook.sh");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\necho \"$CRYSTAL_HOOK $CRYSTAL_WORKTREE_BRANCH $(pwd) $1 $2\" >> {}\n",
            log.display()
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&hook, permissions).unwrap();
    hook
}

#[test]
fn worktree_hooks_in_git_config_run_once_crystal_makes_or_removes_a_worktree() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "app");
    let repo_arg = repo.to_str().unwrap();
    let log = dir.join("hooks.log");
    let hook = recording_hook(dir, &log);
    let hook_arg = hook.to_str().unwrap();
    git(&repo, &["config", "crystal.worktreeCreateHook", hook_arg]);
    git(&repo, &["config", "crystal.worktreeDeleteHook", hook_arg]);

    crystal.ok(&["worktree", "create", "fix", "-C", repo_arg]);
    let project = repo.canonicalize().unwrap();
    let fix = dir.join("app.worktrees/fix").canonicalize().unwrap();
    let created = format!(
        "worktree-create fix {} {} {}",
        project.display(),
        project.display(),
        fix.display()
    );
    eventually("the create hook has run", || {
        std::fs::read_to_string(&log).is_ok_and(|log| log.contains(&created))
    });

    let out = crystal
        .command(&["worktree", "rm", "fix"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let deleted = format!(
        "worktree-delete fix {} {} {}",
        project.display(),
        project.display(),
        fix.display()
    );
    eventually("the delete hook has run", || {
        std::fs::read_to_string(&log).is_ok_and(|log| log.contains(&deleted))
    });

    // One that fails says so, in an event, and the worktree stays made.
    let failing = dir.join("failing.sh");
    std::fs::write(&failing, "#!/bin/sh\necho 'no port for it' >&2\nexit 4\n").unwrap();
    let mut permissions = std::fs::metadata(&failing).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&failing, permissions).unwrap();
    git(
        &repo,
        &[
            "config",
            "crystal.worktreeCreateHook",
            failing.to_str().unwrap(),
        ],
    );
    crystal.ok(&["worktree", "create", "spike", "-C", repo_arg]);
    eventually("the failure is an event", || {
        let events = crystal.ok(&["events", "--json"]);
        events.lines().any(|line| {
            line.contains("worktree.hook_failed") && line.contains("exit status: 4: no port for it")
        })
    });
    assert!(dir.join("app.worktrees/spike").is_dir());
}

#[test]
fn worktree_move_starts_a_session_again_in_the_worktree() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&[
        "new",
        "-d",
        "-n",
        "here",
        "-c",
        repo_arg,
        "sh",
        "-c",
        "pwd > where; exec sleep 30",
    ]);
    written(&repo.join("where"));

    let said = crystal.ok(&["worktree", "move", "fix", "-n", "here"]);
    assert!(said.contains("here moves into"), "{said}");
    assert!(said.contains("now"), "{said}");
    let fix = crystal.dir.path().join("app.worktrees/fix");
    let started_in = written(&fix.join("where"));
    assert_eq!(
        Path::new(started_in.trim()).canonicalize().unwrap(),
        fix.canonicalize().unwrap()
    );
    eventually("it runs on the worktree's branch", || {
        crystal.row("here").is_some_and(|row| row[4] == "fix")
    });

    // Moved again to where it is, nothing happens.
    let said = crystal.ok(&["worktree", "move", "fix", "-n", "here"]);
    assert!(said.contains("is in"), "{said}");
    let err = crystal.fails(&["worktree", "move", "fix"]);
    assert!(err.contains("-n <session>"), "{err}");
}

#[test]
fn an_agent_moved_mid_turn_moves_once_the_turn_ends_and_carries_on_there() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    let bin = fake_claude(crystal.dir.path());
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = crystal
        .command(&["new", "-n", "agent", "-c", repo_arg, "claude"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());

    // In a conversation, in the middle of a turn.
    let args = written(&repo.join("args"));
    let settings: serde_json::Value = claude_settings(&args);
    let hook = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let transcript = crystal.dir.path().join("abc-123.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    let prompted = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "abc-123",
        "transcript_path": transcript,
    });
    run_hook(&crystal, "agent", hook, &prompted.to_string());
    eventually("the agent is working", || {
        crystal.row("agent").is_some_and(|row| row[1] == "working")
    });
    let pid = crystal.pid("agent");

    // The agent asks for itself, from inside its session.
    let listed: serde_json::Value = serde_json::from_str(&crystal.ok(&["ls", "--json"])).unwrap();
    let id = listed[0]["id"].as_str().unwrap().to_string();
    let out = crystal
        .command(&["worktree", "move", "fix"])
        .env("CRYSTAL_SESSION_ID", &id)
        .env("CRYSTAL_SOCKET", &crystal.socket)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said = String::from_utf8(out.stdout).unwrap();
    assert!(said.contains("End your turn now"), "{said}");
    let fix = crystal.dir.path().join("app.worktrees/fix");
    assert!(fix.is_dir());
    thread::sleep(Duration::from_millis(600));
    assert_eq!(crystal.pid("agent"), pid, "it waits for its turn to end");

    let ended = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "abc-123",
        "transcript_path": transcript,
    });
    run_hook(&crystal, "agent", hook, &ended.to_string());
    let args = written(&fix.join("args"));
    let args: Vec<&str> = args.lines().collect();
    let resume = args.iter().position(|arg| *arg == "--resume").unwrap();
    assert_eq!(args[resume + 1], "abc-123");
    let told = args.last().unwrap();
    assert_eq!(args[args.len() - 2], "--");
    assert!(
        told.starts_with("[crystal] This session has moved"),
        "{told}"
    );
    assert!(told.contains("`fix`"), "{told}");
    let row = crystal.row("agent").unwrap();
    assert_eq!(row[4], "fix");
    assert_ne!(crystal.pid("agent"), pid);
}

#[test]
fn a_worktree_s_line_counts_its_changes_and_a_folded_project_stays_folded() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    // A new file of two lines, not committed.
    std::fs::write(repo.join("notes.txt"), "one\ntwo\n").unwrap();
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);
    let mut tui = crystal.tui();
    tui.shows("⌂ main");
    tui.shows("+1 ±2");

    // `h` folds the project down to its heading, which says what's in it.
    tui.type_keys("h");
    tui.shows("app (1)");
    tui.hides("+1 ±2");
    tui.type_keys("q");
    assert!(tui.exit());

    // Folded it stays, the next time; `l` unfolds it.
    let mut tui = crystal.tui();
    tui.shows("app (1)");
    tui.hides("+1 ±2");
    tui.type_keys("l");
    tui.shows("+1 ±2");
    tui.hides("app (1)");
}

#[test]
fn a_worktree_in_the_middle_of_a_rebase_keeps_its_branch_and_says_so() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    // `fix` and `main` change the same line, so a rebase stops on it.
    std::fs::write(repo.join("f.txt"), "first\n").unwrap();
    git(&repo, &["add", "f.txt"]);
    git(&repo, &["commit", "-q", "-m", "a file"]);
    git(&repo, &["checkout", "-q", "-b", "fix"]);
    std::fs::write(repo.join("f.txt"), "the fix\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "the fix"]);
    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f.txt"), "on main\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "on main"]);
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);
    crystal.ok(&[
        "new", "-n", "fixer", "-c", repo_arg, "-w", "fix", "sleep", "30",
    ]);
    let worktree = crystal.dir.path().join("app.worktrees/fix");
    let rebase = outside_crystal("git")
        .args(["-C", worktree.to_str().unwrap(), "rebase", "main"])
        .envs(PLAIN_GIT)
        .output()
        .unwrap();
    assert!(!rebase.status.success(), "the rebase should stop");

    // git calls the worktree detached meanwhile; crystal keeps its branch
    // and says what it's in the middle of.
    let fixer = listed(&crystal, "fixer");
    assert_eq!(fixer["worktree"]["branch"], "fix", "{fixer}");
    assert_eq!(fixer["worktree"]["in_progress"], "rebase", "{fixer}");
    let tui = crystal.tui();
    tui.shows("⎇ fix · rebasing");
    tui.hides("detached");

    // Aborted, the worktree is on its branch again, which `ls` says at
    // once; a session's line in the sidebar follows as the session changes.
    git(&worktree, &["rebase", "--abort"]);
    let fixer = listed(&crystal, "fixer");
    assert_eq!(fixer["worktree"]["branch"], "fix", "{fixer}");
    assert!(fixer["worktree"].get("in_progress").is_none(), "{fixer}");
}

#[test]
fn a_worktree_claude_code_made_for_itself_is_named_by_its_commit() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    std::fs::create_dir_all(repo.join(".claude/worktrees")).unwrap();
    let own = repo.join(".claude/worktrees/agent-a2d61d640a4ff454d");
    let own_arg = own.to_str().unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "worktree-agent-a2d61d640a4ff454d",
            own_arg,
        ],
    );
    std::fs::write(own.join("new.txt"), "x\n").unwrap();
    git(&own, &["add", "."]);
    git(&own, &["commit", "-q", "-m", "feat: add x"]);
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);
    crystal.ok(&["new", "-n", "fixer", "-c", repo_arg, "-w", "fix", "true"]);
    crystal.ok(&["kill", "fixer"]);

    // Claude Code's worktree is named by what it holds, after the others.
    let mut tui = crystal.tui();
    tui.shows("⎇ claude · feat: add x");
    tui.hides("worktree-agent");
    let screen = tui.text();
    let fix = screen.find("⎇ fix").unwrap();
    let claude = screen.find("⎇ claude").unwrap();
    assert!(fix < claude, "{screen}");

    // It's a worktree like any other: `W` removes it.
    tui.type_keys("jj");
    tui.shows("No sessions in ⎇ worktree-agent-a2d61d640a4ff454d");
    tui.type_keys("W");
    tui.shows("remove worktree worktree-agent-a2d61d640a4ff454d? y/n");
    tui.type_keys("y");
    eventually("the worktree is gone", || !own.exists());
    tui.hides("⎇ claude");
}

#[test]
fn shift_w_on_a_worktree_with_work_in_it_asks_again_before_losing_it() {
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
    tui.shows("fix has uncommitted changes: remove it and lose them? y/n");
    // A no keeps it, and the work in it.
    tui.type_keys("n");
    tui.hides("uncommitted changes");
    assert!(worktree.join("notes.txt").exists());
    tui.shows("· no sessions");

    tui.type_keys("W");
    tui.shows("remove worktree fix? y/n");
    tui.type_keys("y");
    tui.shows("fix has uncommitted changes: remove it and lose them? y/n");
    tui.type_keys("y");
    eventually("the worktree is gone", || !worktree.exists());
    tui.hides("⎇ fix");
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
    let install = |args: &[&str]| crystal.run(args);
    let skill_file = crystal.claude_config_dir().join("skills/crystal/SKILL.md");
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

    // Nor does a daemon starting write over it.
    crystal.ok(&["new", "-d", "-n", "worker", "sleep", "30"]);
    assert_eq!(
        std::fs::read_to_string(&skill_file).unwrap(),
        "my own notes\n"
    );

    assert!(install(&["skill", "--install", "--force"]).status.success());
    assert_eq!(std::fs::read_to_string(&skill_file).unwrap(), printed);
}

#[test]
fn a_daemon_starting_never_installs_the_skill() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-d", "-n", "worker", "sleep", "30"]);
    assert!(!crystal.claude_config_dir().join("skills").exists());
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
    let show = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                                  "params": {"name": "memory_show", "arguments": {"id": 1}}});
    let replies = mcp(&crystal, &repo, &[show]);
    let shown = replies[0]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(shown.contains("from: session fixer"), "{shown}");
}

/// Talks to `crystal mcp` serving the memory of the project `dir` is in:
/// sends it `messages`, a line each, then ends its input, and gives back
/// its replies.
fn mcp(crystal: &Crystal, dir: &Path, messages: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut child = crystal
        .command(&["mcp", "-C", dir.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for message in messages {
        writeln!(input, "{message}").unwrap();
    }
    drop(input);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let replies = String::from_utf8(out.stdout).unwrap();
    replies
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn crystal_mcp_serves_the_project_s_memory_to_claude() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "-k",
        "gotcha",
        "Migrations need the database up",
    ]);
    crystal.ok(&["remember", "-C", repo_dir, "Deploys go out on Tuesdays"]);
    let messages = [
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                           "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                      "clientInfo": {"name": "claude-code", "version": "2"}}}),
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                           "params": {"name": "memory_search",
                                      "arguments": {"query": "migrating"}}}),
    ];
    let replies = mcp(&crystal, &repo, &messages);
    // A notification has no reply.
    assert_eq!(replies.len(), 3);
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "crystal");
    assert_eq!(replies[1]["result"]["tools"][0]["name"], "memory_search");
    let found = replies[2]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(found.starts_with("1 · gotcha · "), "{found}");
    assert!(found.contains("Migrations need the database up"), "{found}");
    assert!(!found.contains("Tuesdays"), "{found}");
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
    // About a file that has changed since, so stale: Claude isn't shown it.
    std::fs::write(repo.join("ledger.rs"), "fn round() {}").unwrap();
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "-f",
        "ledger.rs",
        "Ledger rounding lives in ledger.rs",
    ]);
    std::fs::write(repo.join("ledger.rs"), "fn round_down() {}").unwrap();
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
             - 1 (gotcha) The ledger tests need the database up\n"
        ),
        "{args}"
    );
    assert!(args.contains("memory_search tool"), "{args}");
    assert!(args.contains("crystal remember"), "{args}");
    assert!(!args.contains("rounding"), "{args}");
    assert!(args.ends_with("fix the ledger tests\n"), "{args}");

    // And crystal's MCP server, its tools allowed, to search the rest, given
    // ahead of crystal's settings, so they can't take the prompt.
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(args[0], "--mcp-config");
    let server: serde_json::Value = serde_json::from_str(args[1]).unwrap();
    let server = &server["mcpServers"]["crystal"];
    assert_eq!(server["command"], CRYSTAL);
    assert_eq!(server["args"][2], "mcp");
    assert_eq!(server["args"][4], repo_dir);
    assert_eq!(args[2..4], ["--allowedTools", ALLOWED_WITH_MEMORY]);
    assert_eq!(args[4], "--settings");
}

/// A stand-in for an agent crystal knows, `program`: it writes down the
/// arguments it was started with in `args`, each ended by a NUL, since
/// crystal's notes run over several lines, and waits. Returns the
/// directory to put on the PATH.
fn noting_agent(dir: &Path, program: &str) -> PathBuf {
    let bin = dir.join(format!("{program}-bin"));
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join(program),
        "printf '%s\\0' \"$@\" > args.new && mv args.new args\nsleep 30\n",
    );
    bin
}

/// The arguments a [`noting_agent`] started in `dir` was given.
fn noted_args(dir: &Path) -> Vec<String> {
    let file = dir.join("args");
    eventually("the agent has started", || file.exists());
    let args = std::fs::read_to_string(file).unwrap();
    args.split_terminator('\0').map(String::from).collect()
}

#[test]
fn codex_hears_what_its_project_remembered_in_its_developer_instructions() {
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
    let bin = noting_agent(crystal.dir.path(), "codex");
    let out = crystal
        .command(&[
            "new",
            "-n",
            "coder",
            "-c",
            repo_dir,
            "codex",
            "-c",
            "developer_instructions=\"Keep changes small.\"",
            "--",
            "fix the ledger tests",
        ])
        .env("PATH", path_of(&[&bin]))
        .env("CODEX_HOME", crystal.dir.path().join("codex-home"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Its own instructions, a profile's, then crystal's: its task, and
    // what the memory has, with the commands that read the rest.
    let args = noted_args(&repo);
    assert_eq!(args[0], "-c");
    let told = developer_instructions(&args[1]);
    assert!(
        told.starts_with("Keep changes small.\n\nYou're running inside crystal"),
        "{told}"
    );
    assert!(told.contains("crystal done"), "{told}");
    assert!(
        told.contains("\n- 1 (gotcha) The ledger tests need the database up\n"),
        "{told}"
    );
    assert!(told.contains("`crystal memory show <id>`"), "{told}");
    assert_eq!(args[2..], ["--", "fix the ledger tests"]);
}

#[test]
fn another_agent_hears_what_its_project_remembered_atop_its_first_prompt() {
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
    let bin = noting_agent(crystal.dir.path(), "gemini");
    let out = crystal
        .command(&[
            "new",
            "-n",
            "helper",
            "-c",
            repo_dir,
            "-t",
            "fix the ledger tests",
            "gemini",
        ])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let args = noted_args(&repo);
    assert_eq!(args[0], "-i");
    let prompt = &args[1];
    assert!(
        prompt.starts_with("You're running inside crystal"),
        "{prompt}"
    );
    assert!(prompt.contains("crystal done"), "{prompt}");
    assert!(
        prompt.contains("\n- 1 (gotcha) The ledger tests need the database up\n"),
        "{prompt}"
    );
    assert!(prompt.ends_with("\n\nfix the ledger tests"), "{prompt}");
}

#[test]
fn what_its_worktree_changed_comes_first_in_what_an_agent_is_shown() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    std::fs::write(repo.join("fees.rs"), "fn fee() {}").unwrap();
    git(&repo, &["add", "fees.rs"]);
    git(&repo, &["commit", "-q", "-m", "fees"]);
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "-f",
        "fees.rs",
        "Fees are kept in cents",
    ]);
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "-k",
        "gotcha",
        "The refund tests need the ledger running",
    ]);
    // A worktree that has changed fees.rs, and not committed it yet.
    let worktree = crystal.dir.path().join("app-fees");
    let worktree_dir = worktree.to_str().unwrap();
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "fix/fees", worktree_dir],
    );
    std::fs::write(worktree.join("fees.rs"), "fn fee() { round() }").unwrap();

    let bin = fake_claude(crystal.dir.path());
    let out = crystal
        .command(&[
            "new",
            "-n",
            "agent",
            "-c",
            worktree_dir,
            "claude",
            "fix the refund tests",
        ])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(out.status.success());
    let args = written(&worktree.join("args"));
    assert!(
        args.contains(
            "What this project's earlier sessions learned:\n\
             - 1 (note) Fees are kept in cents [fees.rs]\n\
             - 2 (gotcha) The refund tests need the ledger running\n"
        ),
        "{args}"
    );
}

#[test]
fn memory_marks_an_entry_drifting_and_shows_and_exports_it_in_full() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    for file in ["a.rs", "b.rs"] {
        std::fs::write(repo.join(file), file).unwrap();
    }
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "-k",
        "gotcha",
        "-f",
        "a.rs",
        "-f",
        "b.rs",
        "Refunds wait for the ledger",
    ]);
    std::fs::write(repo.join("a.rs"), "changed").unwrap();

    let listed = crystal.ok(&["memory", "-C", repo_dir]);
    assert!(
        listed.contains("Refunds wait for the ledger  (a.rs, b.rs)  [drifting]"),
        "{listed}"
    );
    let shown = crystal.ok(&["memory", "-C", repo_dir, "show", "1"]);
    assert!(
        shown.starts_with(
            "1 · gotcha · drifting: some of its files have changed since\n\n\
             Refunds wait for the ledger\n"
        ),
        "{shown}"
    );
    assert!(
        shown.contains("\nfiles: a.rs, b.rs\nfrom: you\n"),
        "{shown}"
    );
    assert_eq!(
        crystal.ok(&["memory", "-C", repo_dir, "export"]),
        "# app memory\n\n## 1 · gotcha (drifting)\n\nRefunds wait for the ledger\n\n\
         About `a.rs`, `b.rs`. From you.\n"
    );

    // Once all its files have changed, it's stale.
    std::fs::write(repo.join("b.rs"), "changed").unwrap();
    let listed = crystal.ok(&["memory", "-C", repo_dir]);
    assert!(listed.contains("(a.rs, b.rs)  [stale]"), "{listed}");
    let refused = crystal.fails(&["memory", "-C", repo_dir, "show", "7"]);
    assert!(refused.contains("there's no entry 7"), "{refused}");
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
    // A word's start finds it, and so does another form of it.
    let found = crystal.ok(&["memory", "-C", repo_dir, "search", "deploying"]);
    assert!(found.contains("Deploys go out"), "{found}");
    let found = crystal.ok(&["memory", "-C", repo_dir, "search", "tue"]);
    assert!(found.contains("Deploys go out"), "{found}");
}

#[test]
fn memory_search_keeps_to_a_kind_and_files_and_leaves_the_stale_out() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/ledger.rs"), "ledger").unwrap();
    std::fs::write(repo.join("src/fees.rs"), "fees").unwrap();
    let remember = |args: &[&str]| {
        let mut all = vec!["remember", "-C", repo_dir];
        all.extend(args);
        crystal.ok(&all)
    };
    remember(&[
        "-k",
        "gotcha",
        "-f",
        "src/ledger.rs",
        "The ledger tests need redis",
    ]);
    remember(&[
        "-k",
        "decision",
        "-f",
        "src/fees.rs",
        "Ledger fees are kept in cents",
    ]);
    remember(&["-k", "command", "make ledger runs the ledger tests"]);
    let search = |dir: &str, args: &[&str]| {
        let mut all = vec!["memory", "-C", dir, "search"];
        all.extend(args);
        let found = crystal.ok(&all);
        let mut ids: Vec<u64> = found
            .lines()
            .map(|line| line.split_whitespace().next().unwrap().parse().unwrap())
            .collect();
        ids.sort();
        ids
    };
    assert_eq!(search(repo_dir, &["ledger"]), [1, 2, 3]);
    assert_eq!(search(repo_dir, &["ledger", "-k", "command"]), [3]);
    assert_eq!(search(repo_dir, &["ledger", "-f", "src/ledger.rs"]), [1]);
    assert_eq!(search(repo_dir, &["ledger", "-f", "src"]), [1, 2]);
    // A file is named from where the command runs.
    let src = repo.join("src");
    assert_eq!(
        search(src.to_str().unwrap(), &["ledger", "-f", "fees.rs"]),
        [2]
    );
    assert_eq!(search(repo_dir, &["ledger", "-n", "1"]).len(), 1);

    // Stale, it's left out, unless --all says otherwise.
    std::fs::write(repo.join("src/ledger.rs"), "changed").unwrap();
    assert_eq!(search(repo_dir, &["ledger"]), [2, 3]);
    assert_eq!(search(repo_dir, &["ledger", "--all"]), [1, 2, 3]);
    let found = crystal.ok(&["memory", "-C", repo_dir, "search", "redis", "-a"]);
    assert!(found.contains("[stale]"), "{found}");
}

#[test]
fn memory_lists_by_kind_by_title_and_what_was_forgotten() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    let added = crystal.ok(&[
        "memory",
        "-C",
        repo_dir,
        "add",
        "-k",
        "decision",
        "--title",
        "Fees are kept in cents",
        "Never",
        "store",
        "a",
        "float: rounding loses money.",
    ]);
    assert_eq!(added, "remembered 1\n");
    crystal.ok(&["remember", "-C", repo_dir, "Deploys go out on Tuesdays"]);

    // A list shows an entry by its title; show, all of it.
    let listed = crystal.ok(&["memory", "-C", repo_dir, "list"]);
    assert!(
        listed.contains("decision   now  Fees are kept in cents\n"),
        "{listed}"
    );
    assert!(!listed.contains("Never store"), "{listed}");
    let shown = crystal.ok(&["memory", "-C", repo_dir, "show", "1"]);
    assert!(
        shown
            .contains("\n\nFees are kept in cents\n\nNever store a float: rounding loses money.\n"),
        "{shown}"
    );
    let decisions = crystal.ok(&["memory", "-C", repo_dir, "list", "-k", "decision"]);
    assert_eq!(decisions.lines().count(), 1, "{decisions}");
    let long = "a title ".repeat(20);
    let refused = crystal.fails(&["remember", "-C", repo_dir, "--title", &long]);
    assert!(refused.contains("120 characters at most"), "{refused}");

    // What was forgotten is listed apart until it's remembered again.
    crystal.ok(&["memory", "-C", repo_dir, "rm", "2"]);
    let forgotten = crystal.ok(&["memory", "-C", repo_dir, "list", "--forgotten"]);
    assert_eq!(
        forgotten,
        "   2  note       now  Deploys go out on Tuesdays\n"
    );
    assert_eq!(
        crystal.ok(&["memory", "-C", repo_dir, "list", "--wrong"]),
        forgotten
    );
    assert!(!crystal.ok(&["memory", "-C", repo_dir]).contains("Deploys"));
    crystal.ok(&["remember", "-C", repo_dir, "Deploys go out on Tuesdays"]);
    assert_eq!(
        crystal.ok(&["memory", "-C", repo_dir, "list", "--forgotten"]),
        ""
    );
}

#[test]
fn search_by_meaning_without_its_model_goes_by_words_and_says_how_to_get_it() {
    let (crystal, repo) = crystal_remembering();
    crystal.configure("notify = false\n\n[memory]\nembeddings = true\ndistill = false\n");
    let repo_dir = repo.to_str().unwrap();
    crystal.ok(&["remember", "-C", repo_dir, "Deploys go out on Tuesdays"]);
    let out = crystal
        .command(&["memory", "-C", repo_dir, "search", "deploys"])
        // A cache of the test's own, with no model in it.
        .env("XDG_CACHE_HOME", crystal.dir.path().join("cache"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let found = String::from_utf8_lossy(&out.stdout);
    assert!(found.contains("Deploys go out on Tuesdays"), "{found}");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("`crystal memory embed` does now"), "{said}");
}

#[test]
fn the_same_remembered_again_is_the_one_entry_seen_again() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    assert_eq!(
        crystal.ok(&["remember", "-C", repo_dir, "Fees are kept in cents"]),
        "remembered 1\n"
    );
    assert_eq!(
        crystal.ok(&["remember", "-C", repo_dir, "fees are kept in cents."]),
        "remembered 1 already\n"
    );
    assert_eq!(crystal.ok(&["memory", "-C", repo_dir]).lines().count(), 1);
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
    // A daemon to tell, as there is wherever sessions run.
    crystal.ok(&["new", "-d", "-n", "here", "sleep", "30"]);
    crystal.ok(&["memory", "-C", repo_dir, "promote", "1", "--yes"]);
    assert_eq!(
        std::fs::read_to_string(repo.join("CLAUDE.md")).unwrap(),
        "# app\n\nRun make test.\n\n## Notes\n\n- Fees are kept in cents\n"
    );
    let events = crystal.ok(&["events", "-k", "memory.promoted"]);
    assert!(
        events.contains("1 (note) Fees are kept in cents → ") && events.contains("CLAUDE.md"),
        "{events}"
    );
}

#[test]
fn an_entry_whose_files_have_all_changed_is_told_of_as_a_task_closes() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    std::fs::write(repo.join("Makefile"), "test:\n\tcargo test\n").unwrap();
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "-f",
        "Makefile",
        "make test runs the tests",
    ]);
    crystal.ok(&[
        "new", "-d", "-n", "fixer", "-c", repo_dir, "-t", "fix it", "sleep", "30",
    ]);
    std::fs::write(repo.join("Makefile"), "test:\n\tcargo nextest run\n").unwrap();
    crystal.ok(&["done", "-n", "fixer", "moved to nextest"]);
    let stale = || crystal.ok(&["events", "-k", "memory.stale"]);
    eventually("the entry is told of as stale", || {
        stale().contains("make test runs the tests")
    });
    // Once.
    crystal.ok(&[
        "new", "-d", "-n", "again", "-c", repo_dir, "-t", "more", "sleep", "30",
    ]);
    crystal.ok(&["done", "-n", "again", "nothing"]);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(stale().lines().count(), 1, "{}", stale());
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

#[test]
fn enter_in_the_tui_s_memory_opens_an_entry_s_file_in_the_editor() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    std::fs::write(repo.join("ledger.rs"), "fn ledger() {}").unwrap();
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "-f",
        "ledger.rs",
        "The ledger tests need redis",
    ]);
    crystal.ok(&["new", "-n", "agent", "-c", repo_dir, "sleep", "30"]);
    // An editor that notes the file it was asked to open, and where.
    let editor = crystal.dir.path().join("editor");
    let edited = crystal.dir.path().join("edited");
    script(
        &editor,
        "printf '%s %s\\n' \"$PWD\" \"$1\" > \"$EDITED\"\nsleep 30\n",
    );

    let mut tui = crystal.attach_with_env(
        &[],
        &[
            ("EDITOR", editor.to_str().unwrap()),
            ("EDITED", edited.to_str().unwrap()),
        ],
    );
    tui.shows("agent");
    tui.type_keys("m");
    tui.shows("enter edit its file");
    tui.type_keys("\r");
    let repo = std::fs::canonicalize(&repo).unwrap();
    assert_eq!(written(&edited), format!("{} ledger.rs\n", repo.display()));
    tui.shows("typing into ledger.rs");
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

/// The arguments every background task's `claude` gets, by default.
const PRINT_ARGS: &str = "-p --input-format stream-json --output-format stream-json --verbose \
                          --permission-prompt-tool stdio --max-budget-usd 5";

/// The crystal commands a Claude Code session or task is allowed to run
/// without asking, by default in these tests, memory being off.
/// The rules for crystal's own commands a Claude Code session crystal starts
/// is given, as `crystal_commands` in daemon.rs makes them: its sessions'
/// always, then each plugin's while it's on.
macro_rules! session_rules {
    () => {
        "Bash(crystal ls:*),Bash(crystal new:*),Bash(crystal send:*),Bash(crystal wait:*),\
         Bash(crystal read:*),Bash(crystal result:*),Bash(crystal interrupt:*),\
         Bash(crystal events:*),Bash(crystal rename:*),Bash(crystal report:*),\
         Bash(crystal notify:*),Bash(crystal layout),Bash(crystal layout --json),\
         Bash(crystal layout export:*),Bash(crystal pane split:*),Bash(crystal pane close:*)"
    };
}

macro_rules! task_and_flow_rules {
    () => {
        "Bash(crystal done:*),Bash(crystal task:*),Bash(crystal tasks),Bash(crystal tasks --all),\
         Bash(crystal tasks show:*),Bash(crystal tasks log:*),Bash(crystal tasks new:*),\
         Bash(crystal tasks start:*),Bash(crystal flow),Bash(crystal flow --json),\
         Bash(crystal flow run:*),Bash(crystal flow wait:*),Bash(crystal flow show:*),\
         Bash(crystal flow defs:*),Bash(crystal flow retry:*)"
    };
}

macro_rules! backlog_and_handoff_rules {
    () => {
        "Bash(crystal backlog add:*),Bash(crystal backlog),Bash(crystal backlog --all),\
         Bash(crystal backlog list:*),Bash(crystal backlog show:*),Bash(crystal backlog edit:*),\
         Bash(crystal backlog export),Bash(crystal backlog done:*),\
         Bash(crystal backlog reopen:*),Bash(crystal backlog start:*),Bash(crystal handoff:*)"
    };
}

/// With every plugin on but memory, as the tests' own config has them.
const ALLOWED: &str = concat!(
    session_rules!(),
    ",",
    task_and_flow_rules!(),
    ",",
    backlog_and_handoff_rules!()
);

/// A stand-in for a background task's `claude -p`, speaking stream-json:
/// it reads prompts on its standard input, one JSON line each, and answers
/// each as a turn. For each prompt it notes its arguments and the prompt in
/// `runs`, a line each (`<args> -- <prompt>`), then writes what a short turn
/// of Claude writes: some text, a tool and its answer, waiting for the test
/// to make `finish-<run>` before its answer and result. The cost it gives
/// is what its process has cost so far, $0.0421 a turn; its answer takes
/// 24,040 tokens of the 200,000 its model takes, 12%. A prompt with `ASK`
/// in it asks for a permission instead, writes the answer it gets to
/// `answers` and goes on; one with `SLOW` works until it's interrupted.
/// With `FAKE_FAIL` set its result says it failed; with `FAKE_CRASH` it
/// writes an error and exits before saying anything; with `FAKE_ONE_TURN`
/// it exits after its first turn. Returns the directory to put on the
/// PATH.
fn print_claude(dir: &Path) -> PathBuf {
    let bin = dir.join("print-bin");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join("claude"),
        r#"turns=0
while IFS= read -r line; do
    case "$line" in *'"type":"user"'*) ;; *) continue ;; esac
    prompt=$(printf '%s\n' "$line" | sed 's/.*"content":"\([^"]*\)".*/\1/')
    printf '%s -- %s\n' "$*" "$prompt" >> runs
    run=$(grep -c -- ' -- ' runs)
    turns=$((turns + 1))
    if [ -n "$FAKE_CRASH" ]; then
        echo 'Error: Invalid API key' >&2
        exit 1
    fi
    echo '{"type":"system","subtype":"init","session_id":"conv-1","cwd":"/x","model":"m"}'
    case "$prompt" in
    *ASK*)
        echo '{"type":"control_request","request_id":"perm-'"$run"'","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo test"},"tool_use_id":"t1"}}'
        while IFS= read -r answer; do
            case "$answer" in *'"perm-'"$run"'"'*) break ;; esac
        done
        echo "$answer" >> answers
        case "$answer" in
        *'"behavior":"allow"'*) echo '{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"test result: ok. 3 passed","is_error":false}]}}' ;;
        *) echo '{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"the user said no","is_error":true}]}}' ;;
        esac
        ;;
    *SLOW*)
        echo '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Working slowly."}]}}'
        while IFS= read -r request; do
            case "$request" in *'"subtype":"interrupt"'*) break ;; esac
        done
        id=$(printf '%s\n' "$request" | sed 's/.*"request_id":"\([^"]*\)".*/\1/')
        echo '{"type":"control_response","response":{"subtype":"success","request_id":"'"$id"'","response":{}}}'
        echo '{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"conv-1","total_cost_usd":0.01,"duration_ms":100}'
        continue
        ;;
    *)
        echo '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Looking at the tests."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}'
        echo '{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"test result: ok. 3 passed","is_error":false}]}}'
        while [ ! -e "finish-$run" ]; do sleep 0.05; done
        ;;
    esac
    echo '{"type":"assistant","parent_tool_use_id":null,"message":{"model":"m","content":[{"type":"text","text":"All green on run '"$run"'."}],"usage":{"input_tokens":10,"cache_read_input_tokens":24000,"output_tokens":30}}}'
    if [ -n "$FAKE_FAIL" ]; then
        echo '{"type":"result","subtype":"error_max_turns","is_error":true,"session_id":"conv-1","total_cost_usd":0.01,"duration_ms":900}'
        continue
    fi
    cost=$(awk "BEGIN { print $turns * 0.0421 }")
    echo '{"type":"result","subtype":"success","is_error":false,"result":"All green on run '"$run"'.","session_id":"conv-1","total_cost_usd":'"$cost"',"duration_ms":3200,"permission_denials":[{"tool_name":"Bash","tool_use_id":"t9","tool_input":{"command":"rm -rf build"}}],"modelUsage":{"m":{"contextWindow":200000}}}'
    if [ -n "$FAKE_ONE_TURN" ]; then exit 0; fi
done
"#,
    );
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

    // Claude was asked for its events, and given the prompt on its input.
    assert_eq!(
        runs(dir, 1),
        [format!(
            "{PRINT_ARGS} --allowedTools {ALLOWED} -- fix the tests"
        )]
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

    // How full its conversation is, as Claude said.
    let context = &listed(&crystal, "fixer")["context"];
    assert_eq!(context["tokens"], 24_040);
    assert_eq!(context["window"], 200_000);
    let card = crystal.ok(&["tasks", "show", "fixer"]);
    assert!(
        card.contains("  context   24k of 200k tokens, 12%\n"),
        "{card}"
    );
}

#[test]
fn a_follow_up_goes_to_the_same_claude_one_run_at_a_time() {
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
    // The claude still there takes it, in the conversation it's in: it
    // wasn't started again to resume it.
    let runs = runs(dir, 2);
    assert_eq!(
        runs[1],
        format!("{PRINT_ARGS} --allowedTools {ALLOWED} --model opus -- now the docs")
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

    // One whose run failed says so.
    let dir = crystal.dir.path().join("failing");
    std::fs::create_dir(&dir).unwrap();
    finish_run(&dir, 1);
    let out = crystal
        .command(&[
            "task",
            "--wait",
            "-n",
            "broken",
            "-c",
            dir.to_str().unwrap(),
            "fix the tests",
        ])
        .env("PATH", &path)
        .env("FAKE_FAIL", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "broken\nfailed\n");
}

#[test]
fn a_task_whose_run_fails_closes_failed_and_a_follow_up_carries_on() {
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

    // Claude says the run failed: its task closes failed, and the task
    // stays, at rest, for a follow-up.
    let failing = start("failing", "FAKE_FAIL");
    finish_run(&failing, 1);
    eventually("its task has closed, and it's done", || {
        let row = crystal.row("failing").unwrap();
        row[8] == "✗ error max turns" && row[1] == "done"
    });
    shows_on_screen(&crystal, "failing", "✗ failed · error max turns");
    let result: serde_json::Value =
        serde_json::from_str(&crystal.ok(&["result", "failing", "--json"])).unwrap();
    assert_eq!(result["failed"], true);
    eventually("its task is in the history, failed", || {
        crystal
            .ok(&["tasks", "--all"])
            .contains("failed     failing")
    });
    // Its claude was let go: a follow-up starts another, in the same
    // conversation, and opens the task again.
    eventually("its claude has gone", || {
        listed(&crystal, "failing")["pid"].is_null()
    });
    crystal.ok(&["send", "failing", "try again"]);
    assert_eq!(
        runs(&failing, 2)[1],
        format!("{PRINT_ARGS} --resume conv-1 --allowedTools {ALLOWED} -- try again")
    );
    eventually("its task is open again", || {
        crystal
            .ok(&["tasks", "--all"])
            .contains("running    failing")
    });

    // Claude crashes before it says anything: there's no claude to carry
    // on, and the task ends.
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
    keep_claude_transcript(&crystal);

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
    // What it did before is drawn again from Claude Code's transcript, and
    // how full its conversation is read from it.
    shows_on_screen(&crystal, "fixer", "What this task did before");
    shows_on_screen(&crystal, "fixer", "> fix the tests");
    shows_on_screen(&crystal, "fixer", "All green on run 1.");
    eventually("how full its conversation is is read again", || {
        listed(&crystal, "fixer")["context"]["tokens"] == 24_040
    });
    assert_eq!(runs(dir, 1).len(), 1);

    crystal.ok(&["send", "fixer", "carry on"]);
    assert_eq!(
        runs(dir, 2)[1],
        format!("{PRINT_ARGS} --resume conv-1 --allowedTools {ALLOWED} -- carry on")
    );
}

#[test]
fn a_follow_up_once_its_claude_has_gone_resumes_the_conversation() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    let out = crystal
        .command(&["task", "-n", "fixer", "fix the tests"])
        .env("PATH", &path)
        .env("FAKE_ONE_TURN", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    finish_run(dir, 1);
    eventually("the task is done", || status(&crystal, "fixer") == "done");
    eventually("its claude has gone", || {
        listed(&crystal, "fixer")["pid"].is_null()
    });

    crystal.ok(&["send", "fixer", "and the docs"]);
    assert_eq!(
        runs(dir, 2)[1],
        format!("{PRINT_ARGS} --resume conv-1 --allowedTools {ALLOWED} -- and the docs")
    );
}

/// Starts a background task called `name` on `prompt`, its claude found on
/// `path`.
fn start_task(crystal: &Crystal, path: &str, name: &str, prompt: &str) {
    let out = crystal
        .command(&["task", "-n", name, prompt])
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The answers the fake claude has been given, a line each.
fn answers(dir: &Path, count: usize) -> Vec<String> {
    let file = dir.join("answers");
    eventually(&format!("claude has had {count} answers"), || {
        std::fs::read_to_string(&file).is_ok_and(|answers| answers.lines().count() == count)
    });
    let answers = std::fs::read_to_string(&file).unwrap();
    answers.lines().map(String::from).collect()
}

#[test]
fn a_permission_a_task_asks_for_waits_on_the_user_until_they_answer() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    start_task(&crystal, &path, "fixer", "ASK first");

    eventually("the task waits on the user", || {
        status(&crystal, "fixer") == "waiting"
    });
    let asking = &listed(&crystal, "fixer")["asking"];
    assert_eq!(asking["tool"], "Bash");
    assert_eq!(asking["gist"], "cargo test");
    shows_on_screen(&crystal, "fixer", "⚠ Bash cargo test · waiting on you");
    let show = crystal.ok(&["tasks", "show", "fixer"]);
    assert!(
        show.contains("asking    Bash cargo test: crystal answer t1 y|n|always"),
        "{show}"
    );
    // Asking, it takes no follow-up: that's for after it's answered.
    let refused = crystal.fails(&["send", "fixer", "hurry up"]);
    assert!(
        refused.contains(
            "agent_blocked: fixer is asking to use Bash: cargo test. Answer it first, \
             with `crystal answer fixer y|n|always`"
        ),
        "{refused}"
    );

    // Always lets it run, and keeps a rule for calls like it.
    crystal.ok(&["answer", "t1", "always"]);
    let answer: serde_json::Value = serde_json::from_str(&answers(dir, 1)[0]).unwrap();
    let response = &answer["response"]["response"];
    assert_eq!(response["behavior"], "allow");
    assert_eq!(response["updatedInput"]["command"], "cargo test");
    assert_eq!(
        response["updatedPermissions"][0]["rules"][0]["ruleContent"],
        "cargo test:*"
    );
    assert_eq!(
        response["updatedPermissions"][0]["destination"],
        "localSettings"
    );
    shows_on_screen(&crystal, "fixer", "└ allowed always · Bash(cargo test:*)");
    let logged = events(
        &crystal,
        &["-n", "fixer", "-k", "run.asking", "-k", "run.answered"],
    );
    assert_eq!(names(&logged), ["run.asking", "run.answered"]);
    assert_eq!(logged[0]["run"]["asking"]["gist"], "cargo test");
    assert_eq!(logged[1]["run"]["decision"], "always");
    eventually("the task is done", || status(&crystal, "fixer") == "done");
    assert!(listed(&crystal, "fixer")["asking"].is_null());
    let late = crystal.fails(&["answer", "fixer", "y"]);
    assert!(late.contains("isn't asking for anything"), "{late}");

    // No tells Claude why, and it carries on.
    crystal.ok(&["send", "fixer", "ASK again"]);
    eventually("the task waits on the user again", || {
        status(&crystal, "fixer") == "waiting"
    });
    crystal.ok(&["answer", "fixer", "n", "-m", "not on main"]);
    let answer: serde_json::Value = serde_json::from_str(&answers(dir, 2)[1]).unwrap();
    let response = &answer["response"]["response"];
    assert_eq!(response["behavior"], "deny");
    assert_eq!(response["message"], "not on main");
    shows_on_screen(&crystal, "fixer", "└ denied");
    eventually("the task is done", || status(&crystal, "fixer") == "done");
}

#[test]
fn an_interrupted_run_leaves_its_task_open_waiting_on_the_user() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    start_task(&crystal, &path, "fixer", "go SLOW");
    eventually("the task is working", || {
        status(&crystal, "fixer") == "working"
    });
    let idle = crystal.fails(&["interrupt", "nobody"]);
    assert!(
        idle.contains("there's no task or session called nobody"),
        "{idle}"
    );

    crystal.ok(&["interrupt", "fixer"]);
    eventually("the task waits on the user", || {
        status(&crystal, "fixer") == "waiting"
    });
    eventually("the log says why", || {
        let logged = events(
            &crystal,
            &["-n", "fixer", "-k", "run.interrupted", "-k", "task.*"],
        );
        names(&logged) == ["task.opened", "run.interrupted", "task.waiting"]
    });
    shows_on_screen(&crystal, "fixer", "interrupted");
    let tasks = crystal.ok(&["tasks"]);
    assert!(tasks.starts_with("t1    waiting    fixer"), "{tasks}");
    let again = crystal.fails(&["interrupt", "fixer"]);
    assert!(again.contains("isn't working on anything"), "{again}");

    // A follow-up gets it going again, on the same claude.
    crystal.ok(&["send", "fixer", "carry on"]);
    assert_eq!(
        runs(dir, 2)[1],
        format!("{PRINT_ARGS} --allowedTools {ALLOWED} -- carry on")
    );
    finish_run(dir, 2);
    eventually("the task is done", || status(&crystal, "fixer") == "done");
    assert!(crystal.ok(&["tasks"]).starts_with("t1    done       fixer"));
}

#[test]
fn past_the_daily_budget_no_new_run_starts() {
    let crystal = Crystal::new();
    crystal.configure(
        "notify = false\n\n[plugins]\nmemory = false\n\n\
         [tasks]\nmax_budget_usd = 2\ndaily_budget_usd = 0.04\n",
    );
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    start_task(&crystal, &path, "fixer", "fix the tests");
    // Each claude is held to the budget a task has.
    assert!(
        runs(dir, 1)[0].contains("--max-budget-usd 2 "),
        "{:?}",
        runs(dir, 1)
    );
    finish_run(dir, 1);
    eventually("the task is done", || status(&crystal, "fixer") == "done");

    let refused = crystal
        .command(&["task", "-n", "more", "and more"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(
        err.contains("background tasks have spent $0.04 today")
            && err.contains("past the daily budget of $0.04"),
        "{err}"
    );
    assert!(crystal.row("more").is_none());
    let follow_up = crystal.fails(&["send", "fixer", "again"]);
    assert!(follow_up.contains("daily_budget_usd"), "{follow_up}");

    let tui = crystal.tui();
    tui.shows("$0.04 today · over $0.04");
}

#[test]
fn y_answers_a_task_from_the_sidebar_and_ctrl_c_stops_its_run_from_its_pane() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    start_task(&crystal, &path, "fixer", "ASK first");
    eventually("the task waits on the user", || {
        status(&crystal, "fixer") == "waiting"
    });

    let mut tui = crystal.tui();
    tui.shows("⚠ Bash cargo test");
    tui.shows("y allow");
    tui.type_keys("y");
    let answer = &answers(dir, 1)[0];
    assert!(answer.contains(r#""behavior":"allow""#), "{answer}");
    assert!(!answer.contains("updatedPermissions"), "{answer}");
    // Watched as it ends, its turn has been seen: the task says it's done.
    eventually("the task is done", || {
        crystal.row("fixer").unwrap()[8] == "✓ All green on run 1."
    });

    crystal.ok(&["send", "fixer", "go SLOW"]);
    eventually("the task is working", || {
        status(&crystal, "fixer") == "working"
    });
    tui.type_keys("\r");
    tui.shows("ctrl+c stop the run");
    tui.type_keys("\x03");
    eventually("the task waits on the user", || {
        status(&crystal, "fixer") == "waiting"
    });
}

#[test]
fn space_gives_a_task_a_follow_up_from_the_sidebar() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    start_task(&crystal, &path, "fixer", "ASK first");
    eventually("the task waits on the user", || {
        status(&crystal, "fixer") == "waiting"
    });

    let mut tui = crystal.tui();
    tui.shows("⚠ Bash cargo test");
    // Asking, it takes no follow-up: the box says why, and keeps it.
    tui.type_keys(" ");
    tui.shows("Reply · fixer");
    tui.type_keys("now the docs\r");
    tui.shows("agent_blocked: fixer is asking to use Bash");
    tui.shows("now the docs");
    tui.type_keys("\x1b");
    tui.hides("Reply · fixer");
    tui.type_keys("y");
    eventually("the task is done", || {
        crystal.row("fixer").unwrap()[8] == "✓ All green on run 1."
    });

    tui.type_keys(" ");
    tui.shows("Reply · fixer");
    tui.type_keys("now the docs\r");
    tui.shows("sent to fixer");
    tui.hides("Reply · fixer");
    assert_eq!(
        runs(dir, 2)[1],
        format!("{PRINT_ARGS} --allowedTools {ALLOWED} -- now the docs")
    );
}

#[test]
fn a_relative_socket_path_names_the_same_socket_for_the_daemon() {
    let crystal = Crystal::new();
    let mut new = outside_crystal(CRYSTAL);
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
    .env("XDG_CONFIG_HOME", crystal.dir.path())
    .env("CLAUDE_CONFIG_DIR", crystal.claude_config_dir());
    let out = new.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(crystal.dir.path().join("relative.sock").exists());

    let mut stop = outside_crystal(CRYSTAL);
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
/// the JSON given, and `pr list --state merged` with what's in
/// `gh-merged.json` beside it (none, until a test writes some), `pr view`,
/// `pr diff` and `issue view` with a pull request and an issue of its own,
/// and takes comments and edits. It writes each call it gets into
/// `gh-calls` beside it, and what it's given on its standard input into
/// `gh-input`. While `gh-hold-issues` is there, an `issue list` reads the
/// issues as they are and waits to answer. Returns the directory to put
/// first on the PATH.
fn fake_gh(dir: &Path, pull_requests: &str, issues: &str) -> PathBuf {
    let bin = dir.join("gh-bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(dir.join("gh-prs.json"), pull_requests).unwrap();
    std::fs::write(dir.join("gh-merged.json"), "[]").unwrap();
    std::fs::write(dir.join("gh-issues.json"), issues).unwrap();
    std::fs::write(dir.join("gh-pr.json"), PULL_REQUEST_READ).unwrap();
    std::fs::write(dir.join("gh-issue.json"), ISSUE_READ).unwrap();
    std::fs::write(dir.join("forge.diff"), PULL_REQUEST_DIFF).unwrap();
    let dir = dir.display();
    script(
        &bin.join("gh"),
        &format!(
            r#"echo "$*" >> "{dir}/gh-calls"
case "$*" in
    "pr view --web "*) ;;
    "pr list --state merged "*) cat "{dir}/gh-merged.json" ;;
    "pr list "*) cat "{dir}/gh-prs.json" ;;
    "pr view "*) cat "{dir}/gh-pr.json" ;;
    "pr diff "*) cat "{dir}/forge.diff" ;;
    "issue list "*)
        listed=$(cat "{dir}/gh-issues.json")
        while [ -f "{dir}/gh-hold-issues" ]; do sleep 0.05; done
        printf '%s\n' "$listed" ;;
    "issue view "*) cat "{dir}/gh-issue.json" ;;
    "pr comment "*|"issue comment "*|"issue edit "*) cat > "{dir}/gh-input" ;;
    *) echo "unexpected: $*" >&2; exit 1 ;;
esac
"#
        ),
    );
    bin
}

/// What the fake `gh` reads a pull request as.
const PULL_REQUEST_READ: &str = r#"{
    "baseRefName": "main",
    "body": "Sends you home after login.",
    "statusCheckRollup": [
        {"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"}
    ],
    "comments": [
        {"author": {"login": "bo"}, "body": "Does it keep the query string?", "createdAt": "2026-10-02T10:00:00Z"}
    ],
    "reviews": [
        {"author": {"login": "cy"}, "body": "", "state": "APPROVED", "submittedAt": "2026-10-02T11:00:00Z"}
    ]
}"#;

/// What the fake `gh` reads an issue as.
const ISSUE_READ: &str = r#"{
    "body": "The login page sends you back to itself.",
    "comments": [
        {"author": {"login": "bo"}, "body": "Me too, on Safari.", "createdAt": "2026-10-02T10:00:00Z"}
    ]
}"#;

/// What the fake forges say a pull request changes.
const PULL_REQUEST_DIFF: &str = "diff --git a/login.rs b/login.rs
index 3b18e51..a1b2c3d 100644
--- a/login.rs
+++ b/login.rs
@@ -1,3 +1,3 @@
 fn after_login() {
-    redirect(\"/login\");
+    redirect(\"/home\");
 }
";

/// A stand-in for GitLab's `glab`, like [`fake_gh`]: it answers `mr list`
/// and `issue list` with the JSON given, `mr list --merged` with what's in
/// `glab-merged.json`, `mr view`, `mr diff` and `issue view` with a merge
/// request and an issue of its own, and takes notes. It writes each call it
/// gets into `glab-calls` beside it, and what it's given on its standard
/// input into `glab-input`.
fn fake_glab(dir: &Path, merge_requests: &str, issues: &str) -> PathBuf {
    let bin = dir.join("glab-bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(dir.join("glab-mrs.json"), merge_requests).unwrap();
    std::fs::write(dir.join("glab-merged.json"), "[]").unwrap();
    std::fs::write(dir.join("glab-issues.json"), issues).unwrap();
    std::fs::write(dir.join("glab-mr.json"), MERGE_REQUEST_READ).unwrap();
    std::fs::write(dir.join("forge.diff"), PULL_REQUEST_DIFF).unwrap();
    let issue = r#"{"description": "Keys never rotate.", "Notes": []}"#;
    std::fs::write(dir.join("glab-issue.json"), issue).unwrap();
    let dir = dir.display();
    script(
        &bin.join("glab"),
        &format!(
            r#"echo "$*" >> "{dir}/glab-calls"
case "$*" in
    "mr list --merged"*) cat "{dir}/glab-merged.json" ;;
    "mr list"*) cat "{dir}/glab-mrs.json" ;;
    "mr view"*) cat "{dir}/glab-mr.json" ;;
    "mr diff"*) cat "{dir}/forge.diff" ;;
    "mr note"*|"issue update"*) cat > "{dir}/glab-input" ;;
    "issue list"*) cat "{dir}/glab-issues.json" ;;
    "issue view"*) cat "{dir}/glab-issue.json" ;;
    "issue note"*) ;;
    *) echo "unexpected: $*" >&2; exit 1 ;;
esac
"#
        ),
    );
    bin
}

/// What the fake `glab` reads a merge request as.
const MERGE_REQUEST_READ: &str = r#"{
    "iid": 57, "target_branch": "main", "description": "Sends you home after login.",
    "head_pipeline": {"id": 3, "status": "failed"},
    "Discussions": [
        {"notes": [{"author": {"username": "bo"}, "body": "Why here?",
                    "created_at": "2026-10-02T10:00:00Z", "system": false}]},
        {"notes": [{"author": {"username": "cy"}, "body": "approved this merge request",
                    "created_at": "2026-10-02T11:00:00Z", "system": true}]}
    ]
}"#;

/// The calls the fake `gh` or `glab` has had, each a line, once one has
/// come.
fn calls(file: &Path) -> String {
    std::fs::read_to_string(file).unwrap_or_default()
}

/// A repository called `app` whose origin is on github.com, as far as
/// crystal can tell.
fn github_repo(dir: &Path) -> PathBuf {
    let repo = git_repo(dir, "app");
    local_origin(dir, &repo, "https://github.com/acme/app.git");
    repo
}

/// Gives `repo` an origin at `url`, which git goes to a bare repository in
/// `dir` for, with `main` pushed: a new worktree fetches from its origin,
/// and a test never reaches the forge.
fn local_origin(dir: &Path, repo: &Path, url: &str) {
    let origin = dir.join("origin.git");
    let origin_arg = origin.to_str().unwrap();
    git(dir, &["init", "-q", "--bare", "-b", "main", origin_arg]);
    git(repo, &["remote", "add", "origin", url]);
    git(
        repo,
        &["config", &format!("url.{origin_arg}.insteadOf"), url],
    );
    git(repo, &["push", "-q", "origin", "main"]);
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

const OPEN_PULL_REQUEST: &str = r#"[{"number": 57, "title": "Fix the login redirect",
    "author": {"login": "ana"}, "headRefName": "fix-login", "isDraft": false,
    "isCrossRepository": false, "headRepositoryOwner": {"login": "acme"},
    "reviewDecision": "APPROVED", "statusCheckRollup": [], "updatedAt": "2026-10-02T09:30:00Z",
    "url": "https://github.com/acme/app/pull/57"}]"#;

#[test]
fn capital_o_reads_a_pull_request_shows_its_diff_and_comments_on_it() {
    let crystal = Crystal::new();
    let repo = github_repo(crystal.dir.path());
    let bin = fake_gh(crystal.dir.path(), OPEN_PULL_REQUEST, NO_ISSUES);
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("▸ planner");
    tui.type_keys("O");
    tui.shows("pull requests · app");
    tui.shows("Fix the login redirect");
    // The highlighted one, read whole: its checks, text and conversation.
    tui.shows("ana wants to merge fix-login into main");
    tui.shows("✓ build");
    tui.shows("Sends you home after login.");
    tui.shows("Does it keep the query string?");
    tui.shows("cy approved");

    // Its diff, then back to the list.
    tui.type_keys("\x04");
    tui.shows("pull request #57");
    tui.shows("redirect(\"/home\");");
    // A file marked reviewed stays marked the next time it's read.
    tui.type_keys("r");
    tui.shows("1 reviewed");
    tui.type_keys("\x1b");
    tui.shows("pull requests · app");
    tui.type_keys("\x04");
    tui.shows("1 reviewed");
    tui.type_keys("\x1b");
    tui.shows("pull requests · app");

    tui.type_keys("\x03");
    tui.shows("comment on #57");
    tui.type_keys("Looks right.\r");
    tui.shows("commented on #57");
    let gh_calls = crystal.dir.path().join("gh-calls");
    assert!(
        calls(&gh_calls)
            .lines()
            .any(|call| call == "pr comment 57 --body-file -"),
        "{}",
        calls(&gh_calls)
    );
    let input = std::fs::read_to_string(crystal.dir.path().join("gh-input")).unwrap();
    assert_eq!(input, "Looks right.");
    // It's read again, with the comment in it.
    eventually("the pull request is read again", || {
        let reads = calls(&gh_calls);
        reads
            .lines()
            .filter(|call| call.starts_with("pr view 57 "))
            .count()
            == 2
    });

    tui.type_keys("\x0f");
    eventually("gh is asked to open the pull request", || {
        calls(&gh_calls)
            .lines()
            .any(|call| call == "pr view --web 57")
    });
}

#[test]
fn pull_requests_are_marked_merged_or_conflicting_and_counted_in_the_top_bar() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = github_repo(dir);
    let open = r#"[
        {"number": 57, "title": "Fix the login redirect", "author": {"login": "ana"},
         "headRefName": "fix-login", "isDraft": false, "mergeable": "CONFLICTING",
         "updatedAt": "2026-10-02T09:30:00Z", "url": "https://github.com/acme/app/pull/57"},
        {"number": 58, "title": "Dark mode", "author": {"login": "bo"},
         "headRefName": "dark", "isDraft": true, "mergeable": "MERGEABLE",
         "updatedAt": "2026-10-01T09:30:00Z", "url": "https://github.com/acme/app/pull/58"}
    ]"#;
    let issues = r#"[
        {"number": 7, "title": "Dark mode", "labels": [], "updatedAt": "2026-09-01T10:00:00Z",
         "author": {"login": "bo"}, "url": "https://github.com/acme/app/issues/7"},
        {"number": 42, "title": "Fix login redirect", "labels": [],
         "updatedAt": "2026-10-01T10:00:00Z", "author": {"login": "ana"},
         "url": "https://github.com/acme/app/issues/42"}
    ]"#;
    let bin = fake_gh(dir, open, issues);
    let merged = r#"[{"number": 41, "title": "Faster startup", "author": {"login": "cy"},
        "headRefName": "startup", "updatedAt": "2026-09-30T09:30:00Z",
        "url": "https://github.com/acme/app/pull/41"}]"#;
    std::fs::write(dir.join("gh-merged.json"), merged).unwrap();
    let repo_arg = repo.to_str().unwrap();
    // The worktree of a pull request that has merged.
    crystal.ok(&[
        "new", "-n", "starter", "-c", repo_arg, "-w", "startup", "sleep", "30",
    ]);

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("#41 merged");
    tui.shows("1 session · 2 prs · 2 issues");

    tui.type_keys("O");
    tui.shows("pull requests · app");
    tui.shows("2 open");
    tui.shows("Fix the login redirect  conflicts");
    tui.shows("conflicts with main: it can't merge as it stands");
    tui.shows("Dark mode  draft");
    tui.shows("Faster startup  merged");

    // Ctrl+R asks the forge again.
    let gh_calls = dir.join("gh-calls");
    let listed = || {
        calls(&gh_calls)
            .lines()
            .filter(|call| call.starts_with("pr list --state open"))
            .count()
    };
    let before = listed();
    tui.type_keys("\x12");
    eventually("gh is asked for the list again", || listed() > before);
    tui.type_keys("\x1b");
    tui.hides("pull requests · app");

    // Hidden in the settings, drafts leave the list and the count. Theirs
    // is the last row of the first tab.
    tui.type_keys(",");
    tui.shows("○ notifications");
    tui.type_keys(&"j".repeat(20));
    tui.shows("○ hide drafts");
    tui.type_keys(" ");
    tui.shows("● hide drafts");
    let config = || std::fs::read_to_string(crystal.config_file()).unwrap();
    eventually("hiding drafts is written down", || {
        config().contains("[forge]\nhide_draft_prs = true")
    });
    // The count follows at once, the settings still open; an `O` typed
    // before the Esc has closed them would be read with it as Alt+O.
    tui.shows("1 session · 1 pr · 2 issues");
    tui.type_keys("\x1b");
    tui.hides("hide drafts");
    tui.type_keys("O");
    tui.shows("1 open · 1 draft hidden");
    tui.shows("Fix the login redirect");
    tui.hides("Dark mode");
}

#[test]
fn enter_on_a_pull_request_starts_a_session_in_its_worktree_forks_too() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    // The project's origin, on this machine but named as GitHub: git goes
    // to the one, and crystal's forge reads the other.
    let origin = dir.join("origin.git");
    git(
        dir,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    let repo = git_repo(dir, "app");
    git(
        &repo,
        &["remote", "add", "origin", "https://github.com/acme/app.git"],
    );
    let instead = format!("url.{}.insteadOf", origin.display());
    git(
        &repo,
        &["config", &instead, "https://github.com/acme/app.git"],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["checkout", "-q", "-b", "fix-login"]);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "send them home"],
    );
    git(&repo, &["push", "-q", "origin", "fix-login"]);
    let fix = git(&repo, &["rev-parse", "HEAD"]);
    // A fork's commit, which the project only has under refs/pull.
    git(&repo, &["checkout", "-q", "main"]);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "from a fork"],
    );
    let forked = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["push", "-q", "origin", "HEAD:refs/pull/58/head"]);
    git(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
    git(&repo, &["branch", "-q", "-D", "fix-login"]);

    let pull_requests = r#"[
        {"number": 57, "title": "Fix the login redirect", "author": {"login": "bo"},
         "headRefName": "fix-login", "isCrossRepository": false,
         "url": "https://github.com/acme/app/pull/57"},
        {"number": 58, "title": "Dark mode", "author": {"login": "ana"},
         "headRefName": "main", "isCrossRepository": true, "headRepositoryOwner": {"login": "ana"},
         "url": "https://github.com/acme/app/pull/58"}
    ]"#;
    let gh = fake_gh(dir, pull_requests, NO_ISSUES);
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);

    let claude = fake_claude(dir);
    let path = path_of(&[&gh, &claude]);
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("▸ planner");
    tui.type_keys("O");
    tui.shows("Fix the login redirect");
    tui.type_keys("\r");
    tui.shows("New session · app ⎇ fix-login");
    tui.shows("Work on pull request #57: Fix the login");
    tui.type_keys("\r");
    tui.shows("⎇ fix-login");
    let worktree = dir.join("app.worktrees/fix-login");
    let args = written(&worktree.join("args"));
    assert!(args.contains("Work on pull request #57"), "{args}");
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), fix);
    let upstream = git(&worktree, &["rev-parse", "--abbrev-ref", "@{upstream}"]);
    assert_eq!(upstream.trim(), "origin/fix-login");

    // The fork's `main` is never the project's: it goes under its owner.
    tui.type_keys("\x1c");
    tui.type_keys("O");
    tui.shows("Dark mode");
    tui.type_keys("\x1b[B\r");
    tui.shows("New session · app ⎇ ana/main");
    tui.type_keys("\r");
    tui.shows("⎇ ana/main");
    let fork = dir.join("app.worktrees/ana-main");
    written(&fork.join("args"));
    assert_eq!(git(&fork, &["rev-parse", "HEAD"]), forked);
    let merge = git(&repo, &["config", "branch.ana/main.merge"]);
    assert_eq!(merge.trim(), "refs/pull/58/head");
}

#[test]
fn slash_finds_a_pull_request_a_project_with_nothing_running_and_keeps_to_a_status() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = github_repo(dir);
    let bin = fake_gh(dir, OPEN_PULL_REQUEST, NO_ISSUES);
    let api = git_repo(dir, "api");
    crystal.ok(&["project", "add", api.to_str().unwrap()]);
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("▸ planner");

    // A pull request, by a word of its title, opens in the view.
    tui.type_keys("/redirect");
    tui.shows("#57 Fix the login");
    tui.shows("1 match");
    tui.type_keys("\r");
    tui.shows("pull requests · app");
    tui.shows("ana wants to merge fix-login into main");
    tui.type_keys("\x1b");
    tui.shows("▸ planner");

    // A project with nothing running: the selection goes onto it.
    tui.type_keys("/api");
    tui.shows("1 match");
    tui.type_keys("\r");
    tui.shows("No sessions in ⎇ main");
    tui.shows("api ▸ main");

    // Tab keeps to one status: nothing waits, and the shell is idle.
    tui.type_keys("/\t");
    tui.shows("find waiting:");
    tui.shows("0 matches");
    tui.type_keys("\x1b[Z\x1b[Z\x1b[Z");
    tui.shows("find idle:");
    eventually("the idle shell is found", || {
        sidebar_of(&tui.text()).contains("planner")
    });
    tui.shows("1 match");
}

#[test]
fn an_issue_takes_a_comment_and_a_new_title() {
    let crystal = Crystal::new();
    let repo = github_repo(crystal.dir.path());
    let issues = r#"[{"number": 42, "title": "Fix login redirect", "labels": [],
        "updatedAt": "2026-10-01T10:00:00Z", "author": {"login": "ana"},
        "url": "https://github.com/acme/app/issues/42"}]"#;
    let bin = fake_gh(crystal.dir.path(), "[]", issues);
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["new", "-n", "planner", "-c", repo_arg, "sleep", "30"]);

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("▸ planner");
    // The issues are listed as the TUI starts, counted on the top bar, and
    // again as the view opens, which says so meanwhile. That list is held
    // until after the title is changed below.
    tui.shows("1 issue");
    let hold = crystal.dir.path().join("gh-hold-issues");
    std::fs::write(&hold, "").unwrap();
    tui.type_keys("i");
    tui.shows("The login page sends you back to itself.");
    tui.shows("Me too, on Safari.");
    tui.shows("asking GitHub");

    tui.type_keys("\x03");
    tui.shows("comment on #42");
    tui.type_keys("Same here\r");
    tui.shows("commented on #42");
    let gh_calls = crystal.dir.path().join("gh-calls");
    let input = crystal.dir.path().join("gh-input");
    assert!(calls(&gh_calls).contains("issue comment 42 --body-file -"));
    assert_eq!(std::fs::read_to_string(&input).unwrap(), "Same here");

    tui.type_keys("\x05");
    tui.shows("edit issue #42");
    tui.type_keys(" on Safari\r");
    tui.shows("updated issue #42");
    tui.shows("Fix login redirect on Safari");
    let edit = "issue edit 42 --title=Fix login redirect on Safari --body-file -";
    assert!(calls(&gh_calls).contains(edit), "{}", calls(&gh_calls));
    assert_eq!(
        std::fs::read_to_string(&input).unwrap(),
        "The login page sends you back to itself."
    );

    // The list asked before the change, with the old title, lands after
    // it: the new title stays.
    std::fs::remove_file(&hold).unwrap();
    tui.hides("asking GitHub");
    assert!(tui.text().contains("Fix login redirect on Safari"));
}

#[test]
fn on_gitlab_merge_requests_and_issues_go_through_glab() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    local_origin(crystal.dir.path(), &repo, "git@gitlab.com:acme/app.git");
    let merge_requests = r#"[{"iid": 57, "title": "Draft: Fix the login redirect", "draft": true,
        "author": {"username": "ana"}, "source_branch": "fix-login",
        "source_project_id": 7, "target_project_id": 7,
        "updated_at": "2026-10-02T09:30:00.000Z",
        "web_url": "https://gitlab.com/acme/app/-/merge_requests/57"}]"#;
    let issues = r#"[{"iid": 8, "title": "Rotate keys", "labels": ["security"],
        "updated_at": "2026-10-01T10:00:00Z", "author": {"username": "di"},
        "web_url": "https://gitlab.com/acme/app/-/work_items/8"}]"#;
    let glab = fake_glab(crystal.dir.path(), merge_requests, issues);
    let merged = r#"[{"iid": 50, "title": "Faster startup", "author": {"username": "cy"},
        "source_branch": "startup", "source_project_id": 7, "target_project_id": 7,
        "updated_at": "2026-09-30T09:30:00Z",
        "web_url": "https://gitlab.com/acme/app/-/merge_requests/50"}]"#;
    std::fs::write(crystal.dir.path().join("glab-merged.json"), merged).unwrap();
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

    let path = format!("{}:{}", glab.display(), std::env::var("PATH").unwrap());
    let mut tui = crystal.attach_with_env(&[], &[("PATH", &path)]);
    tui.shows("!57 draft");
    tui.type_keys("O");
    tui.shows("merge requests · app");
    tui.shows("Fix the login redirect  draft");
    tui.shows("Faster startup  merged");
    tui.shows("✗ pipeline");
    tui.shows("Why here?");
    tui.shows("cy approved");

    tui.type_keys("\x03");
    tui.shows("comment on !57");
    tui.type_keys("LGTM\r");
    tui.shows("commented on !57");
    let glab_calls = crystal.dir.path().join("glab-calls");
    assert!(
        calls(&glab_calls)
            .lines()
            .any(|call| call == "mr note create 57"),
        "{}",
        calls(&glab_calls)
    );
    let input = crystal.dir.path().join("glab-input");
    assert_eq!(std::fs::read_to_string(&input).unwrap(), "LGTM");

    // Esc on its own, not the start of an Alt+i.
    tui.type_keys("\x1b");
    tui.hides("merge requests · app");
    tui.type_keys("i");
    tui.shows("Rotate keys");
    tui.shows("Keys never rotate.");
    tui.type_keys("\x03");
    tui.type_keys("On it\r");
    tui.shows("commented on #8");
    assert!(
        calls(&glab_calls).contains("issue note 8 --message=On it"),
        "{}",
        calls(&glab_calls)
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

    // `/` filters the files by a few letters of their paths.
    tui.type_keys("/tdo");
    tui.shows("1 match");
    tui.hides("notes.md");
    tui.shows("+ write the tests");
    // Enter keeps the filter; Esc then clears it.
    tui.type_keys("\r");
    tui.type_keys("\x1b");
    tui.shows("notes.md");
    tui.hides("1 match");

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

/// A worktree with a markdown guide holding a diagram, some code, and a
/// file git ignores.
fn repo_with_docs(dir: &Path) -> PathBuf {
    let repo = git_repo(dir, "app");
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("target")).unwrap();
    std::fs::write(
        repo.join("docs/guide.md"),
        "# The guide\n\nHow a frame is **drawn**:\n\n```mermaid\nflowchart LR\n  read --> draw\n```\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn draw() {}\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    std::fs::write(repo.join("target/junk.txt"), "build output\n").unwrap();
    repo
}

#[test]
fn big_e_browses_the_worktree_as_a_tree_with_each_file_previewed() {
    let crystal = Crystal::new();
    let repo = repo_with_docs(crystal.dir.path());
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
            ("SSH_TTY", "/dev/ttys999"),
            ("EDITOR", editor.to_str().unwrap()),
            ("EDITED", edited.to_str().unwrap()),
        ],
    );
    tui.shows("agent");
    tui.type_keys("E");
    tui.shows("tree · 3 files");
    // Folded, directories first; what git ignores isn't there.
    tui.shows("▸ docs");
    tui.shows("▸ src");
    tui.shows(".gitignore");
    assert!(!tui.text().contains("target"), "{}", tui.text());

    // Typing filters the tree down to the guide, which shows as a page
    // with its diagram drawn.
    tui.type_keys("guide");
    tui.shows("tree · 1 of 3 files");
    tui.shows("▾ docs");
    tui.hides("▸ src");
    tui.shows("The guide");
    tui.shows("How a frame is drawn:");
    tui.shows("│ read ├──▶│ draw │");
    tui.shows("mermaid · flowchart");

    // Its source, then its path copied.
    tui.type_keys("\x12");
    tui.shows("```mermaid");
    tui.type_keys("\x19");
    tui.copies("docs/guide.md");
    tui.shows("copied docs/guide.md");

    // Esc clears the filter, and the tree is folded as it was.
    tui.type_keys("\x1b");
    tui.shows("▸ src");
    tui.shows("tree · 3 files");

    // Into src, and its file opens in the editor.
    tui.type_keys("\x1b[B\x1b[C\x1b[B");
    tui.shows("pub fn draw() {}");
    tui.type_keys("\x05");
    assert_eq!(written(&edited), "src/lib.rs\n");
    tui.shows("typing into lib.rs");
}

#[test]
fn dragging_the_tree_browsers_border_makes_the_tree_wider() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    std::fs::write(repo.join("a-file-with-a-long-name-indeed.txt"), "hi\n").unwrap();
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
    tui.type_keys("E");
    tui.shows("a-file-with-a-long-…");
    // The border is the 25th column, from the third row down, counting
    // from 1 as the mouse does: take it, drag it to the 50th, and let go.
    tui.type_keys("\x1b[<0;25;3M\x1b[<32;50;3M\x1b[<0;50;3m");
    tui.shows("a-file-with-a-long-name-indeed.txt");
}

#[test]
fn mermaid_draws_a_diagram_and_fails_on_one_it_cant() {
    let crystal = Crystal::new();
    let draw = |args: &[&str], diagram: &str| {
        let mut child = crystal
            .command(&["mermaid"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(diagram.as_bytes()).unwrap();
        drop(stdin);
        child.wait_with_output().unwrap()
    };

    let out = draw(&[], "sequenceDiagram\n  Alice->>Bob: hello\n");
    assert!(out.status.success());
    let drawn = String::from_utf8(out.stdout).unwrap();
    assert!(drawn.contains("│ Alice │  │ Bob │"), "{drawn}");
    assert!(drawn.contains("├────────▶│"), "{drawn}");

    let out = draw(&["--ascii", "--width", "40"], "flowchart LR\n  a --> b\n");
    let drawn = String::from_utf8(out.stdout).unwrap();
    assert_eq!(drawn, "+---+   +---+\n| a +-->| b |\n+---+   +---+\n");

    let out = draw(&[], "pie\n  \"a\": 1\n");
    assert!(!out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "pie\n  \"a\": 1\n");
    let why = String::from_utf8(out.stderr).unwrap();
    assert!(
        why.contains("not drawn: pie diagrams are not drawn in a terminal"),
        "{why}"
    );
}

#[test]
fn r_marks_a_file_reviewed_until_it_changes_and_t_lists_files_as_a_tree() {
    let crystal = Crystal::new();
    let repo = repo_with_work(crystal.dir.path());
    std::fs::create_dir_all(repo.join("src/tui")).unwrap();
    std::fs::write(repo.join("src/tui/app.rs"), "fn app() {}\n").unwrap();
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
    tui.shows("uncommitted changes · 3 files +3 −0");
    // Reading starts at the first file, and goes on to the next.
    tui.shows("+ - ask about fees");
    tui.type_keys("r");
    tui.shows("1 reviewed");
    tui.shows("+ fn app() {}");

    // The mark is there the next time the diff is read…
    tui.type_keys("q");
    tui.hides("1 reviewed");
    tui.type_keys("d");
    tui.shows("1 reviewed");
    // …until the file changes again.
    tui.type_keys("q");
    std::fs::write(repo.join("notes.md"), "# Notes\n- ask about refunds\n").unwrap();
    tui.type_keys("d");
    tui.shows("ask about refunds");
    tui.hides("1 reviewed");

    // As a tree, and still a tree the next time.
    tui.type_keys("t");
    tui.shows("▾ src/tui");
    tui.type_keys("q");
    tui.hides("▾ src/tui");
    tui.type_keys("d");
    tui.shows("▾ src/tui");
}

#[test]
fn capital_g_finds_text_in_the_files_and_edits_one_at_its_line() {
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
    // An editor that notes what it was asked to open.
    let editor = crystal.dir.path().join("editor");
    let edited = crystal.dir.path().join("edited");
    script(&editor, "printf '%s\\n' \"$@\" > \"$EDITED\"\nsleep 30\n");

    let mut tui = crystal.attach_with_env(
        &[],
        &[
            ("EDITOR", editor.to_str().unwrap()),
            ("EDITED", edited.to_str().unwrap()),
        ],
    );
    tui.shows("agent");
    tui.type_keys("G");
    tui.shows("find in files");
    tui.shows("type 2 letters or more");
    // A new file git doesn't know about yet is searched too.
    tui.type_keys("TESTS");
    tui.shows("nothing found");
    tui.type_keys("\x7f\x7f\x7f\x7f\x7fledger");
    tui.shows("1 line in 1 file");
    tui.shows("refund.rs");
    tui.shows("3  ledger.write(total);");

    tui.type_keys("\r");
    assert_eq!(written(&edited), "+3\nrefund.rs\n");
    tui.shows("typing into refund.rs");
}

#[test]
fn capital_b_makes_a_branch_and_switches_back_stashing_the_changes() {
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
    // A stash is a commit, and git wants to know whose.
    let mut tui = crystal.attach_with_env(
        &[],
        &[
            ("GIT_AUTHOR_NAME", "crystal"),
            ("GIT_AUTHOR_EMAIL", "crystal@example.com"),
            ("GIT_COMMITTER_NAME", "crystal"),
            ("GIT_COMMITTER_EMAIL", "crystal@example.com"),
        ],
    );
    tui.shows("agent");
    tui.type_keys("B");
    tui.shows("switch branch · 2 uncommitted changes");
    tui.shows("● fee");
    // The branch it can switch to is selected, with its last commit.
    tui.shows("just now · refunds");

    // A name no branch has makes one, and the changes come along.
    tui.type_keys("spike");
    tui.shows("no such branch");
    tui.shows("Enter makes this branch from fee");
    tui.type_keys("\r");
    tui.shows("the worktree is on spike");
    assert_eq!(git(&repo, &["branch", "--show-current"]), "spike\n");
    assert!(repo.join("todo.txt").exists());

    // Going back to main asks what's to become of them.
    tui.type_keys("B");
    tui.shows("● spike");
    tui.type_keys("main\r");
    tui.shows("spike has 2 uncommitted changes");
    tui.type_keys("s");
    tui.shows("the worktree is on main · changes stashed as");
    assert_eq!(git(&repo, &["branch", "--show-current"]), "main\n");
    assert!(!repo.join("todo.txt").exists());
    let stashed = git(&repo, &["stash", "list"]);
    assert!(
        stashed.contains("crystal: spike before switching to main"),
        "{stashed}"
    );
}

#[test]
fn capital_b_fetches_the_remotes_so_their_new_branches_are_listed() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "app");
    let origin = dir.join("origin.git");
    let origin_arg = origin.to_str().unwrap();
    git(dir, &["init", "-q", "--bare", "-b", "main", origin_arg]);
    git(&repo, &["remote", "add", "origin", origin_arg]);
    git(&repo, &["push", "-q", "origin", "main"]);
    // Pushed from somewhere else: the repository hasn't fetched it.
    git(&origin, &["branch", "from-elsewhere", "main"]);
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
    tui.type_keys("B");
    tui.shows("switch branch");
    tui.shows("origin/from-elsewhere");
    tui.hides("fetching…");

    // Ctrl+R fetches again, however lately it did.
    git(&origin, &["branch", "later", "main"]);
    tui.type_keys("\x12");
    tui.shows("origin/later");
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
        tasks.starts_with("t1    done       fixer")
            && tasks.contains("fix the tests — did what was asked"),
        "{tasks}"
    );
}

#[test]
fn done_refuses_while_the_task_s_worktree_is_in_the_middle_of_a_merge() {
    let crystal = Crystal::new();
    let repo = git_repo(crystal.dir.path(), "app");
    let repo_arg = repo.to_str().unwrap();
    let task = |name: &str| {
        crystal.ok(&[
            "new", "-d", "-n", name, "-c", repo_arg, "-t", "fix it", "sleep", "30",
        ]);
    };
    task("fixer");
    task("quitter");
    // A merge stopped on conflicts leaves MERGE_HEAD behind until it's
    // finished or aborted.
    let head = git(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join(".git/MERGE_HEAD"), head).unwrap();

    let refused = crystal.fails(&["done", "-n", "fixer", "fixed it"]);
    assert!(
        refused.contains("fixer's worktree is in the middle of a merge")
            && refused.contains("crystal done --failed"),
        "{refused}"
    );
    assert_eq!(crystal.row("fixer").unwrap()[8], "fix it");
    // Failed, it closes whatever the worktree is in the middle of.
    crystal.ok(&["done", "-n", "quitter", "--failed", "the merge conflicts"]);
    assert_eq!(crystal.row("quitter").unwrap()[8], "✗ the merge conflicts");

    // The merge aborted, the task closes done.
    std::fs::remove_file(repo.join(".git/MERGE_HEAD")).unwrap();
    crystal.ok(&["done", "-n", "fixer", "fixed it"]);
    assert_eq!(crystal.row("fixer").unwrap()[8], "✓ fixed it");
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
        tasks.contains("failed     given") && tasks.contains("tidy up — no time"),
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
    let events = crystal.ok(&["events", "-k", "task.reminded"]);
    assert!(
        events.contains("agent") && events.contains("fix the tests"),
        "{events}"
    );

    // …and once is enough: it may have its reasons to leave the task open,
    // like a question for the user, so it waits on them.
    run_hook(&crystal, "agent", &hook, stop);
    assert_eq!(crystal.row("agent").unwrap()[1], "waiting");
    assert_eq!(crystal.row("agent").unwrap()[8], "fix the tests");
    assert_eq!(listed(&crystal, "agent")["task"]["waiting"], true);
    assert!(crystal.ok(&["tasks"]).starts_with("t1    waiting    agent"));

    // Back at work once the user answers.
    run_hook(
        &crystal,
        "agent",
        &hook,
        r#"{"hook_event_name":"UserPromptSubmit"}"#,
    );
    assert_eq!(crystal.row("agent").unwrap()[1], "working");
    assert!(crystal.ok(&["tasks"]).starts_with("t1    running    agent"));
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
    assert!(crystal.ok(&["tasks"]).starts_with("t1    running    fixer"));

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
fn a_task_made_to_wait_is_pending_until_it_starts() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    let made = crystal
        .command(&[
            "tasks",
            "new",
            "--background",
            "--no-launch",
            "-n",
            "later",
            "fix the tests",
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&made.stdout), "t1\n");
    let tasks = crystal.ok(&["tasks"]);
    assert!(tasks.starts_with("t1    pending    -  "), "{tasks}");
    let show = crystal.ok(&["tasks", "show", "t1"]);
    assert!(show.starts_with("t1  pending  fix the tests\n"), "{show}");
    assert!(!dir.join("runs").exists(), "nothing has started on it");

    // Started, it keeps its number.
    let started = crystal
        .command(&["tasks", "start", "1"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&started.stdout), "later\n");
    assert_eq!(
        runs(dir, 1),
        [format!(
            "{PRINT_ARGS} --allowedTools {ALLOWED} -- fix the tests"
        )]
    );
    assert!(crystal.ok(&["tasks"]).starts_with("t1    running    later"));
    finish_run(dir, 1);
    eventually("the task is done", || status(&crystal, "later") == "done");

    // Its log is how it stands, then its transcript.
    let log = crystal.ok(&["tasks", "log", "t1"]);
    assert!(log.starts_with("t1  done  fix the tests\n"), "{log}");
    assert!(
        log.contains("> fix the tests") && log.contains("All green on run 1."),
        "{log}"
    );
    let again = crystal.fails(&["tasks", "start", "t1"]);
    assert!(
        again.contains("there's no task t1 waiting to start"),
        "{again}"
    );
    let logged = events(&crystal, &["-k", "task.*"]);
    assert_eq!(
        names(&logged),
        ["task.opened", "task.started", "task.closed"]
    );
    assert_eq!(logged[0]["task"]["pending"], true);
    assert_eq!(logged[0]["task"]["id"], 1);
    assert_eq!(logged[1]["session"]["name"], "later");
}

#[test]
fn a_task_carries_acceptance_criteria_under_its_prompt_and_on_its_card() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    std::fs::write(dir.join("done-when.md"), "# Done when\n- [ ] no warnings\n").unwrap();
    let out = crystal
        .command(&[
            "task",
            "-n",
            "fixer",
            "--accept",
            "the tests pass",
            "--accept-file",
            "done-when.md",
            "fix the tests",
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Its first prompt has them under its goal, as Claude reads it.
    let prompt = r"fix the tests\n\nAcceptance criteria:\n- the tests pass\n- no warnings";
    assert_eq!(
        runs(dir, 1),
        [format!("{PRINT_ARGS} --allowedTools {ALLOWED} -- {prompt}")]
    );
    shows_on_screen(&crystal, "fixer", "> - no warnings");
    let card = crystal.ok(&["tasks", "show", "fixer"]);
    assert!(
        card.contains("  accept    the tests pass\n            no warnings\n"),
        "{card}"
    );
    let task = &listed(&crystal, "fixer")["task"];
    assert_eq!(task["goal"], "fix the tests");
    assert_eq!(
        task["accept"],
        serde_json::json!(["the tests pass", "no warnings"])
    );

    // An agent in a terminal has them in its first prompt too, and a task
    // made to wait keeps them until it starts.
    let claude = fake_claude(dir);
    let terminal = path_with(&claude);
    let made = crystal
        .command(&[
            "tasks",
            "new",
            "--no-launch",
            "-n",
            "later",
            "--accept",
            "the docs say so",
            "write the docs",
        ])
        .env("PATH", &terminal)
        .output()
        .unwrap();
    assert!(made.status.success());
    let card = crystal.ok(&["tasks", "show", "t2"]);
    assert!(card.contains("  accept    the docs say so\n"), "{card}");
    let started = crystal
        .command(&["tasks", "start", "t2"])
        .env("PATH", &terminal)
        .output()
        .unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let args = written(&dir.join("args"));
    assert!(
        args.ends_with("write the docs\n\nAcceptance criteria:\n- the docs say so\n"),
        "{args}"
    );
}

#[test]
fn the_settings_give_background_tasks_a_permission_mode_and_rules() {
    let crystal = Crystal::new();
    crystal.configure(
        "notify = false\nname_from_prompt = false\n\n[plugins]\nmemory = false\n\n\
         [sound]\nenabled = false\n\n[mouse]\nscrollbars = false\n\n\
         [terminal]\nshell_mode = \"non_login\"\n\n\
         [tasks]\npermission_mode = \"acceptEdits\"\nallowed_tools = [\"Bash(make:*)\"]\n",
    );
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    start_task(&crystal, &path, "fixer", "fix the tests");
    assert_eq!(
        runs(dir, 1),
        [format!(
            "{PRINT_ARGS} --permission-mode acceptEdits --allowedTools Bash(make:*) {ALLOWED} \
             -- fix the tests"
        )]
    );
    // The task's own arguments win.
    let other = dir.join("other");
    std::fs::create_dir(&other).unwrap();
    let out = crystal
        .command(&[
            "task",
            "-n",
            "planner",
            "-c",
            other.to_str().unwrap(),
            "plan it",
            "--",
            "--permission-mode",
            "plan",
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let run = &runs(&other, 1)[0];
    assert!(
        run.contains("--permission-mode plan") && !run.contains("acceptEdits"),
        "{run}"
    );
}

/// A stand-in for `gh` that knows pull request 57 and issue 7 of
/// acme/app, as `crystal task --pr` and `--issue` ask for them, and writes
/// each call into `gh-calls` beside it.
fn task_gh(dir: &Path) -> PathBuf {
    let bin = dir.join("task-gh-bin");
    std::fs::create_dir(&bin).unwrap();
    let calls = dir.join("gh-calls");
    script(
        &bin.join("gh"),
        &format!(
            r#"echo "$*" >> "{}"
case "$*" in
    "pr view 57 --json "*) echo '{{"number": 57, "title": "Fix the login redirect", "headRefName": "fix-login", "isCrossRepository": false, "state": "OPEN", "url": "https://github.com/acme/app/pull/57"}}' ;;
    "issue view 7 --json "*) echo '{{"number": 7, "title": "Login loops", "url": "https://github.com/acme/app/issues/7"}}' ;;
    *) echo "no such thing: $*" >&2; exit 1 ;;
esac
"#,
            calls.display()
        ),
    );
    bin
}

#[test]
fn a_task_on_a_pull_request_runs_in_its_worktree_and_is_told_to_read_it_first() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = github_repo(dir);
    git(&repo, &["checkout", "-q", "-b", "fix-login"]);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "send them home"],
    );
    git(&repo, &["push", "-q", "origin", "fix-login"]);
    let fix = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "-q", "main"]);
    git(&repo, &["branch", "-q", "-D", "fix-login"]);
    let gh = task_gh(dir);
    let path = format!("{}:{}", gh.display(), path_with(&print_claude(dir)));
    let repo_arg = repo.to_str().unwrap();

    let out = crystal
        .command(&[
            "task", "-n", "fixer", "-c", repo_arg, "--pr", "57", "--issue", "7",
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "fixer\n");
    // In the pull request's worktree, made for it, its commits fetched.
    let worktree = dir.join("app.worktrees/fix-login");
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), fix);
    let ran = worktree.join("runs");
    eventually("claude has run", || {
        std::fs::read_to_string(&ran).is_ok_and(|runs| runs.contains("-- Work on"))
    });
    let runs = std::fs::read_to_string(&ran).unwrap();
    assert!(
        runs.ends_with(
            "-- Work on pull request #57: Fix the login redirect \
             (https://github.com/acme/app/pull/57)\n"
        ),
        "{runs}"
    );
    assert!(
        runs.contains("Your task is about GitHub pull request #57")
            && runs.contains("`gh pr view 57 --comments`")
            && runs.contains("Your task is for GitHub issue #7, \"Login loops\""),
        "{runs}"
    );
    let card = crystal.ok(&["tasks", "show", "fixer"]);
    assert!(
        card.contains(
            "  pr        #57 Fix the login redirect · https://github.com/acme/app/pull/57\n"
        ) && card.contains("  issue     #7 Login loops · https://github.com/acme/app/issues/7\n"),
        "{card}"
    );
    assert!(card.contains("  where     app fix-login\n"), "{card}");

    // On an issue alone, it runs where it's started, to fix it.
    let out = crystal
        .command(&[
            "tasks",
            "new",
            "--background",
            "-n",
            "issue",
            "-c",
            repo_arg,
            "--issue",
            "7",
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ran = repo.join("runs");
    eventually("claude has run on the issue", || {
        std::fs::read_to_string(&ran)
            .is_ok_and(|runs| runs.contains("-- Fix issue #7: Login loops"))
    });
    let calls = std::fs::read_to_string(dir.join("gh-calls")).unwrap();
    assert!(calls.starts_with("pr view 57 --json "), "{calls}");

    // One the forge doesn't know starts nothing.
    let missing = crystal
        .command(&["task", "-c", repo_arg, "--pr", "99"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("no such thing: pr view 99"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
}

/// Writes the transcript Claude Code keeps of conversation `conv-1` into
/// the test's Claude Code config directory: the prompt, and Claude's
/// answer, with how many tokens it took.
fn keep_claude_transcript(crystal: &Crystal) {
    let kept = crystal.claude_config_dir().join("projects/-work-app");
    std::fs::create_dir_all(&kept).unwrap();
    let lines = [
        r#"{"type":"user","message":{"role":"user","content":"fix the tests"},"isSidechain":false,"sessionId":"conv-1"}"#,
        r#"{"type":"assistant","message":{"model":"m","content":[{"type":"text","text":"All green on run 1."}],"usage":{"input_tokens":10,"cache_read_input_tokens":24000,"output_tokens":30}},"isSidechain":false,"sessionId":"conv-1"}"#,
    ];
    std::fs::write(kept.join("conv-1.jsonl"), lines.join("\n") + "\n").unwrap();
}

#[test]
fn a_background_task_opens_in_a_terminal_in_its_conversation() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    let out = crystal
        .command(&[
            "task",
            "-n",
            "fixer",
            "fix the tests",
            "--",
            "--model",
            "opus",
            "--max-turns",
            "3",
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    finish_run(dir, 1);
    eventually("the task is done", || status(&crystal, "fixer") == "done");
    keep_claude_transcript(&crystal);

    let terminal = path_with(&fake_claude(dir));
    let out = crystal
        .command(&["tasks", "terminal", "t1"])
        .env("PATH", &terminal)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "fixer\n");
    // Claude Code in a terminal, in the task's conversation, with what of
    // its arguments a terminal takes.
    let args = written(&dir.join("args"));
    let args: Vec<&str> = args.lines().collect();
    let resume = args.iter().position(|arg| *arg == "--resume").unwrap();
    assert_eq!(args[resume + 1], "conv-1");
    assert!(
        args.windows(2).any(|pair| pair == ["--model", "opus"]),
        "{args:?}"
    );
    assert!(
        !args.contains(&"--max-turns") && !args.contains(&"-p"),
        "{args:?}"
    );
    let session = listed(&crystal, "fixer");
    assert_eq!(
        session["command"],
        serde_json::json!(["claude", "--model", "opus"])
    );
    assert_ne!(session["front"]["kind"], "task");
    // Its task went with it, as it stood.
    assert_eq!(session["task"]["id"], 1);
    assert_eq!(session["task"]["background"], false);
    let card = crystal.ok(&["tasks", "show", "t1"]);
    assert!(card.starts_with("t1  done  fix the tests\n"), "{card}");
    assert!(card.contains("in a terminal"), "{card}");
    let events = crystal.ok(&["events", "-k", "session.opened_in_terminal"]);
    assert!(events.contains("fixer"), "{events}");
    // What Claude did in the background was told of, a tool at a time.
    let events = crystal.ok(&["events", "-k", "run.tool_use"]);
    assert!(events.contains("Bash cargo test"), "{events}");

    // A session in a terminal is in one already, and a task in the middle
    // of a run is let finish first.
    let refused = crystal.fails(&["tasks", "terminal", "fixer"]);
    assert!(refused.contains("isn't a background task"), "{refused}");
    let slow = dir.join("slow");
    std::fs::create_dir(&slow).unwrap();
    let out = crystal
        .command(&["task", "-n", "slow", "-c", slow.to_str().unwrap(), "SLOW"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    eventually("it's working", || status(&crystal, "slow") == "working");
    let refused = crystal.fails(&["tasks", "terminal", "slow"]);
    assert!(refused.contains("in the middle of a run"), "{refused}");
}

#[test]
fn cancelling_a_task_stops_its_session_and_killing_a_session_cancels_its_task() {
    let crystal = Crystal::new();
    // One waiting to start goes straight to the history.
    assert_eq!(
        crystal.ok(&["tasks", "new", "--no-launch", "tidy up"]),
        "t1\n"
    );
    crystal.ok(&["tasks", "cancel", "t1"]);
    let tasks = crystal.ok(&["tasks"]);
    assert!(
        tasks.contains("t1    cancelled  -")
            && tasks.contains("tidy up — cancelled before it started"),
        "{tasks}"
    );

    // One a session works on stops the session, which stays to be read.
    crystal.ok(&["new", "-d", "-n", "sleeper", "-t", "nap", "sleep", "30"]);
    crystal.ok(&["tasks", "cancel", "sleeper"]);
    eventually("the session has stopped", || {
        status(&crystal, "sleeper") != "running"
    });
    assert_eq!(
        crystal.row("sleeper").unwrap()[8],
        "– cancelled by the user"
    );
    let show = crystal.ok(&["tasks", "show", "t2"]);
    assert!(show.starts_with("t2  cancelled  nap\n"), "{show}");
    let closed = crystal.fails(&["tasks", "cancel", "t2"]);
    assert!(
        closed.contains("sleeper has no task that's open"),
        "{closed}"
    );

    crystal.ok(&["new", "-d", "-n", "doomed", "-t", "dig", "sleep", "30"]);
    crystal.ok(&["kill", "doomed"]);
    let tasks = crystal.ok(&["tasks"]);
    assert!(
        tasks.contains("t3    cancelled  doomed") && tasks.contains("dig — its session was killed"),
        "{tasks}"
    );
    let closed = events(&crystal, &["-k", "task.closed"]);
    let cancelled: Vec<&serde_json::Value> = closed
        .iter()
        .map(|event| &event["task"]["outcome"]["cancelled"])
        .collect();
    assert_eq!(cancelled, [true, true, true]);
    assert!(closed[0].get("session").is_none(), "it never had one");
    let printed = crystal.ok(&["events", "-k", "task.closed", "-n", "doomed"]);
    assert!(
        printed.contains("cancelled: its session was killed"),
        "{printed}"
    );
}

#[test]
fn a_task_whose_session_ends_with_it_open_fails_saying_how() {
    let crystal = Crystal::new();
    crystal.ok(&[
        "new", "-d", "-n", "quitter", "-t", "tidy up", "sh", "-c", "exit 3",
    ]);
    eventually("the task has failed", || {
        let tasks = crystal.ok(&["tasks"]);
        tasks.contains("t1    failed     quitter")
            && tasks.contains("tidy up — its session ended: exited 3")
    });
}

#[test]
fn a_handoff_note_stays_in_the_worktree_and_the_next_agent_there_is_told_of_it() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "app");
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&[
        "new",
        "-d",
        "-n",
        "porter",
        "-c",
        repo_arg,
        "-t",
        "port the codec",
        "sleep",
        "30",
    ]);
    crystal.ok(&[
        "handoff",
        "-n",
        "porter",
        "The fixtures live in tests/fixtures;",
        "cargo test codec runs them",
    ]);
    let file = repo.join(".crystal/handoff.md");
    let notes = std::fs::read_to_string(&file).unwrap();
    let heading = notes.lines().next().unwrap();
    assert!(
        heading.starts_with("## 20") && heading.ends_with(" · porter · task \"port the codec\""),
        "{notes}"
    );
    assert!(
        notes.contains("\nThe fixtures live in tests/fixtures; cargo test codec runs them\n"),
        "{notes}"
    );
    // Kept out of git.
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");

    // Closing the task adds how it went.
    crystal.ok(&["done", "-n", "porter", "ported", "it"]);
    let notes = std::fs::read_to_string(&file).unwrap();
    assert!(
        notes.contains(" · porter · task \"port the codec\" done\nported it\n"),
        "{notes}"
    );

    // The next agent there is told to read them, and what they say.
    let bin = fake_claude(dir);
    let out = crystal
        .command(&["new", "-d", "-n", "next", "-c", repo_arg, "claude"])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let args = written(&repo.join("args"));
    assert!(
        args.contains("This worktree has notes left by the sessions before you"),
        "{args}"
    );
    assert!(args.contains("crystal handoff \"<note>\""), "{args}");
    assert!(args.contains("cargo test codec runs them"), "{args}");
    assert!(args.contains("ported it"), "{args}");

    let logged = events(&crystal, &["-k", "handoff.added"]);
    assert_eq!(logged.len(), 2);
    assert_eq!(logged[1]["handoff"]["note"], "ported it");
    assert_eq!(logged[1]["session"]["name"], "porter");

    // Outside git there's no worktree to keep notes in, and an empty note
    // is no note.
    crystal.ok(&["new", "-d", "-n", "loose", "sleep", "30"]);
    let err = crystal.fails(&["handoff", "-n", "loose", "x"]);
    assert!(err.contains("loose isn't in a git worktree"), "{err}");
    let err = crystal.fails(&["handoff", "-n", "porter", " "]);
    assert!(err.contains("the note is empty"), "{err}");
}

#[test]
fn done_keeps_the_files_it_names_with_the_task_and_refuses_one_outside_its_worktree() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "app");
    let repo_arg = repo.to_str().unwrap();
    std::fs::create_dir(repo.join("docs")).unwrap();
    std::fs::write(repo.join("docs/plan.md"), "the plan\n").unwrap();
    let outside = dir.join("elsewhere.md");
    std::fs::write(&outside, "not here\n").unwrap();
    crystal.ok(&[
        "new",
        "-d",
        "-n",
        "planner",
        "-c",
        repo_arg,
        "-t",
        "write the plan",
        "sleep",
        "30",
    ]);
    crystal.ok(&["handoff", "-n", "planner", "the plan is in docs"]);

    let refused = crystal.fails(&[
        "done",
        "-n",
        "planner",
        "--artifact",
        outside.to_str().unwrap(),
        "wrote it",
    ]);
    assert!(
        refused.contains("isn't in the task's worktree") && refused.contains("still open"),
        "{refused}"
    );
    assert_eq!(crystal.row("planner").unwrap()[8], "write the plan");

    // A path is taken from where the command runs.
    let out = crystal
        .command(&[
            "done",
            "-n",
            "planner",
            "--artifact",
            "docs/plan.md",
            "wrote",
            "it",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let kept = crystal.socket.with_extension("tasks").join("t1");
    assert_eq!(written(&kept.join("plan.md")), "the plan\n");
    let handoff = std::fs::read_to_string(kept.join("handoff.md")).unwrap();
    assert!(
        handoff.contains("the plan is in docs") && handoff.contains("done\nwrote it\n"),
        "{handoff}"
    );

    let show = crystal.ok(&["tasks", "show", "t1"]);
    assert!(show.contains("  kept      "), "{show}");
    assert!(show.contains("t1/plan.md (9 bytes)\n"), "{show}");
    assert!(show.contains("t1/handoff.md ("), "{show}");
    let listed: serde_json::Value =
        serde_json::from_str(&crystal.ok(&["tasks", "--json", "-C", repo_arg])).unwrap();
    let artifacts = &listed[0]["artifacts"];
    assert_eq!(artifacts[0]["kind"], "file");
    assert_eq!(artifacts[0]["name"], "plan.md");
    assert_eq!(artifacts[0]["bytes"], 9);
    assert_eq!(artifacts[1]["kind"], "handoff");

    let logged = events(&crystal, &["-k", "task.artifact"]);
    let names: Vec<&str> = logged
        .iter()
        .map(|event| event["artifact"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["plan.md", "handoff.md"]);
    assert_eq!(logged[0]["task"]["id"], 1);
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

    // The item the bar is on shows its body under the list, and `e`
    // changes its line.
    crystal.ok(&[
        "backlog",
        "-C",
        repo_dir,
        "edit",
        "2",
        "-b",
        "The total is off by a cent.",
    ]);
    tui.type_keys("b");
    tui.shows("The total is off by a cent.");
    tui.type_keys("e");
    tui.shows("#2's line:");
    tui.type_keys(" again\r");
    eventually("#2's line is changed", || {
        crystal.ok(&["backlog", "-C", repo_dir]) == "#2    fix the cart again +\n"
    });
}

#[test]
fn a_backlog_item_has_a_body_and_tags_to_edit_show_filter_and_import() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "shop");
    let repo_dir = repo.to_str().unwrap();
    let backlog = |args: &[&str]| {
        let mut all = vec!["backlog", "-C", repo_dir];
        all.extend(args);
        crystal.ok(&all)
    };
    let added = backlog(&[
        "add",
        "retry",
        "the",
        "webhook",
        "-b",
        "On a timeout only.",
        "-t",
        "payments",
        "-t",
        "#ci",
    ]);
    assert_eq!(added, "#1\n");
    assert_eq!(backlog(&["add", "fix the cart", "-t", "ui"]), "#2\n");
    // A `+` says there's more to it than its line.
    assert_eq!(
        backlog(&[]),
        "#1    retry the webhook +  #payments  #ci\n#2    fix the cart  #ui\n"
    );
    assert_eq!(backlog(&["-t", "ui"]), "#2    fix the cart  #ui\n");
    assert_eq!(
        backlog(&["list", "-t", "ci", "-t", "payments"]),
        "#1    retry the webhook +  #payments  #ci\n"
    );
    assert_eq!(
        backlog(&["show", "1"]),
        "#1  open  retry the webhook\n  tags      #payments #ci\n  added     just now\n\n  \
         On a timeout only.\n"
    );

    backlog(&["edit", "1", "retry", "the", "webhook", "twice"]);
    backlog(&[
        "edit",
        "2",
        "--no-tags",
        "-b",
        "The total is off by a cent.",
    ]);
    let shown: serde_json::Value =
        serde_json::from_str(&backlog(&["show", "2", "--json"])).unwrap();
    assert_eq!(shown["text"], "fix the cart");
    assert_eq!(shown["body"], "The total is off by a cent.");
    assert_eq!(shown["tags"], serde_json::json!([]));
    assert_eq!(shown["tasks"], serde_json::json!([]));
    let refused = crystal.fails(&["backlog", "-C", repo_dir, "edit", "2"]);
    assert!(refused.contains("say what to change"), "{refused}");
    let missing = crystal.fails(&["backlog", "-C", repo_dir, "show", "9"]);
    assert!(
        missing.contains("there's no #9 on the backlog"),
        "{missing}"
    );

    // The export reads back: into its own project it adds nothing, and
    // into another, every item, done or not, with its body and tags.
    backlog(&["done", "2"]);
    let export = backlog(&["export"]);
    assert_eq!(
        export,
        "# shop backlog\n\n\
         - [ ] retry the webhook twice (#1) #payments #ci\n  On a timeout only.\n\
         - [x] fix the cart (#2)\n  The total is off by a cent.\n"
    );
    let file = dir.join("TODO.md");
    std::fs::write(&file, &export).unwrap();
    let file = file.to_str().unwrap();
    assert_eq!(
        backlog(&["import", file]),
        "added nothing\npassed over 2 already on the backlog\n"
    );
    let other = git_repo(dir, "other");
    let other_dir = other.to_str().unwrap();
    assert_eq!(
        crystal.ok(&["backlog", "-C", other_dir, "import", file]),
        "added #1 #2\n"
    );
    assert_eq!(
        crystal.ok(&["backlog", "-C", other_dir, "--all"]),
        "#1    retry the webhook twice +  #payments  #ci\n#2    ✓ fix the cart +\n"
    );
    std::fs::write(dir.join("empty.md"), "# nothing\n\n- not a box\n").unwrap();
    let empty = dir.join("empty.md");
    let refused = crystal.fails(&[
        "backlog",
        "-C",
        other_dir,
        "import",
        empty.to_str().unwrap(),
    ]);
    assert!(refused.contains("no `- [ ]` item"), "{refused}");
}

#[test]
fn a_backlog_item_starts_with_a_profile_in_a_worktree_and_shows_its_tasks() {
    let crystal = Crystal::new();
    crystal.configure(
        "notify = false\n\n[plugins]\nmemory = false\n\n\
         [[profile]]\nname = \"builder\"\nagent = \"claude\"\nmodel = \"opus\"\n\
         prompt = \"Test first.\"\nwhere = \"worktree\"\n",
    );
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "shop");
    let repo_dir = repo.to_str().unwrap();
    crystal.ok(&[
        "backlog",
        "-C",
        repo_dir,
        "add",
        "write the docs",
        "-b",
        "The guide first.",
    ]);
    let refused = crystal.fails(&["backlog", "-C", repo_dir, "start", "1", "-p", "nobody"]);
    assert!(
        refused.contains("there's no profile called nobody"),
        "{refused}"
    );

    let bin = finishing_claude(dir);
    let finish = dir.join("finish");
    let out = crystal
        .command(&[
            "backlog", "-C", repo_dir, "start", "1", "-p", "builder", "-d", "-n", "docs",
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
    // In a worktree named after the item, as the profile says, with its
    // model, and its prompt ahead of the item's line and body.
    let worktree = PathBuf::from(listed(&crystal, "docs")["cwd"].as_str().unwrap());
    assert!(
        worktree.to_str().unwrap().contains("write-the-docs"),
        "{worktree:?}"
    );
    let args = written(&worktree.join("args"));
    assert!(args.contains("\n--model\nopus\n"), "{args}");
    assert!(
        args.ends_with("\n--\nTest first.\n\nwrite the docs\n\nThe guide first.\n"),
        "{args}"
    );
    let shown = crystal.ok(&["backlog", "-C", repo_dir, "show", "1"]);
    assert!(
        shown.contains("  tasks     t1    running, in docs\n"),
        "{shown}"
    );

    std::fs::write(&finish, "").unwrap();
    eventually("the item is ticked", || {
        crystal.ok(&["backlog", "-C", repo_dir]).is_empty()
    });
    let shown = crystal.ok(&["backlog", "-C", repo_dir, "show", "1"]);
    assert!(shown.starts_with("#1  done  write the docs\n"), "{shown}");
    assert!(
        shown.contains("  tasks     t1    done, just now: did what was asked\n"),
        "{shown}"
    );
}

#[test]
fn a_backlog_item_starts_in_the_background_on_a_pull_request() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = github_repo(dir);
    git(&repo, &["checkout", "-q", "-b", "fix-login"]);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "send them home"],
    );
    git(&repo, &["push", "-q", "origin", "fix-login"]);
    git(&repo, &["checkout", "-q", "main"]);
    git(&repo, &["branch", "-q", "-D", "fix-login"]);
    let path = format!(
        "{}:{}",
        task_gh(dir).display(),
        path_with(&print_claude(dir))
    );
    let repo_arg = repo.to_str().unwrap();
    crystal.ok(&["backlog", "-C", repo_arg, "add", "send them home"]);

    let out = crystal
        .command(&[
            "backlog",
            "-C",
            repo_arg,
            "start",
            "1",
            "--pr",
            "57",
            "--background",
            "-n",
            "home",
            "--",
            "--model",
            "haiku",
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "home\n");
    // In the pull request's worktree, told of it, asked what the item says.
    let ran = dir.join("app.worktrees/fix-login/runs");
    eventually("claude has run", || {
        std::fs::read_to_string(&ran).is_ok_and(|runs| runs.ends_with("-- send them home\n"))
    });
    let runs = std::fs::read_to_string(&ran).unwrap();
    assert!(runs.contains("--model haiku"), "{runs}");
    assert!(
        runs.contains("Your task is about GitHub pull request #57"),
        "{runs}"
    );
    let card = crystal.ok(&["tasks", "show", "home"]);
    assert!(card.contains("  backlog   #1\n"), "{card}");
    assert!(card.contains("in the background"), "{card}");
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
        [format!(
            "{PRINT_ARGS} --allowedTools {ALLOWED} -- fix the tests"
        )]
    );
    assert_eq!(crystal.row("task").unwrap()[8], "fix the tests");
    finish_run(dir, 1);
    eventually("the task has closed", || {
        crystal.row("task").unwrap()[8] == "✓ All green on run 1."
    });
}

#[test]
fn a_closed_task_is_kept_in_its_project_s_history_not_its_memory() {
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
    eventually("the task is in the project's history", || {
        crystal
            .ok(&["tasks"])
            .contains("fix the tests — did what was asked")
    });
    let remembered = crystal.ok(&["memory"]);
    assert!(!remembered.contains("did what was asked"), "{remembered}");
}

/// A stand-in for Claude that plays both its parts in a task: as the task,
/// `claude -p --input-format stream-json`, it writes its arguments down one
/// a line in `task-args`, and the prompt it reads in `task-prompt`, then a
/// short run: it found redis down, ran the tests and says so. As the
/// distiller, run with `--json-schema`, it
/// writes its arguments to `distill-args` and its message to
/// `distill-message`, and answers with two entries, one of a kind it may
/// not give. Returns the directory to put on the PATH.
fn distilling_claude(dir: &Path) -> PathBuf {
    let bin = dir.join("distilling-bin");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join("claude"),
        r#"case " $* " in
*" --json-schema "*)
    cat > distill-message
    printf '%s\n' "$@" > distill-args.new && mv distill-args.new distill-args
    echo '{"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{"entries":[{"kind":"gotcha","text":"The ledger tests need redis up","files":["ledger.rs"]},{"kind":"outcome","text":"fixed it","files":[]}]},"total_cost_usd":0.01}'
    ;;
*)
    printf '%s\n' "$@" > task-args.new && mv task-args.new task-args
    while IFS= read -r line; do
        case "$line" in *'"type":"user"'*) ;; *) continue ;; esac
        printf '%s\n' "$line" | sed 's/.*"content":"\([^"]*\)".*/\1/' > task-prompt
        echo '{"type":"system","subtype":"init","session_id":"conv-1"}'
        echo '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Found it: redis was down."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}'
        echo '{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"test result: ok","is_error":false}]}}'
        echo '{"type":"result","subtype":"success","is_error":false,"result":"Fixed: start redis first.","session_id":"conv-1","total_cost_usd":0.04,"duration_ms":100}'
    done
    ;;
esac
"#,
    );
    bin
}

/// What a Claude Code session or task crystal starts is allowed to run
/// without asking, with every plugin on: the crystal commands it's told
/// to run, then the tools of crystal's MCP server.
const ALLOWED_WITH_MEMORY: &str = concat!(
    session_rules!(),
    ",",
    task_and_flow_rules!(),
    ",",
    backlog_and_handoff_rules!(),
    ",Bash(crystal remember:*),Bash(crystal memory),Bash(crystal memory add:*),\
     Bash(crystal memory list:*),Bash(crystal memory search:*),\
     Bash(crystal memory show:*),mcp__crystal__memory_search,mcp__crystal__memory_show"
);

#[test]
fn a_task_may_close_itself_without_asking_whatever_is_off() {
    let crystal = Crystal::new();
    crystal.configure("notify = false\n\n[plugins]\nmemory = false\nbacklog = false\n");
    let repo = git_repo(crystal.dir.path(), "app");
    let bin = distilling_claude(crystal.dir.path());
    start_fixer(&crystal, &repo, &bin);

    eventually("the task starts", || repo.join("task-args").exists());
    let args = written(&repo.join("task-args"));
    let args: Vec<&str> = args.lines().collect();
    let at = args
        .iter()
        .position(|arg| *arg == "--allowedTools")
        .unwrap();
    // Neither memory's commands nor its server, nor the backlog's.
    assert_eq!(
        args[at + 1],
        concat!(
            session_rules!(),
            ",",
            task_and_flow_rules!(),
            ",Bash(crystal handoff:*)"
        )
    );
    assert!(!args.contains(&"--mcp-config"), "{args:?}");
}

/// Starts a task called fixer in `repo`, on the ledger, with `bin` for its
/// PATH.
fn start_fixer(crystal: &Crystal, repo: &Path, bin: &Path) {
    let out = crystal
        .command(&[
            "task",
            "-n",
            "fixer",
            "-c",
            repo.to_str().unwrap(),
            "fix the ledger",
        ])
        .env("PATH", path_of(&[bin]))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_task_is_shown_what_was_learned_and_given_crystal_s_mcp_server() {
    let (crystal, repo) = crystal_remembering();
    crystal.configure("notify = false\n\n[memory]\ndistill = false\n");
    let repo_dir = repo.to_str().unwrap();
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "-k",
        "gotcha",
        "The ledger needs redis",
    ]);
    let bin = distilling_claude(crystal.dir.path());
    start_fixer(&crystal, &repo, &bin);

    let args = written(&repo.join("task-args"));
    let args: Vec<&str> = args.lines().collect();
    let after = |flag: &str| {
        let at = args.iter().position(|arg| *arg == flag).unwrap();
        args[at + 1]
    };
    assert_eq!(after("--allowedTools"), ALLOWED_WITH_MEMORY);
    let server: serde_json::Value = serde_json::from_str(after("--mcp-config")).unwrap();
    let server = &server["mcpServers"]["crystal"];
    assert_eq!(server["command"], CRYSTAL);
    assert_eq!(server["args"][2], "mcp");
    // What it was shown is in its system prompt, by id, for memory_show.
    let prompt = args.join("\n");
    assert!(
        prompt.contains("- 1 (gotcha) The ledger needs redis"),
        "{prompt}"
    );
    assert!(prompt.contains("memory_search tool"), "{prompt}");
    assert_eq!(written(&repo.join("task-prompt")), "fix the ledger\n");

    eventually("the task is done", || {
        crystal
            .ok(&["tasks", "-C", repo_dir])
            .contains("fix the ledger — Fixed: start redis first.")
    });
    let remembered = crystal.ok(&["memory", "-C", repo_dir]);
    assert!(!remembered.contains("start redis first"), "{remembered}");
    // With the distiller off, nothing more is read.
    thread::sleep(Duration::from_millis(300));
    assert!(!repo.join("distill-args").exists());
}

#[test]
fn the_distiller_keeps_what_a_closed_task_learned() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    std::fs::write(repo.join("ledger.rs"), "fn ledger() {}").unwrap();
    let bin = distilling_claude(crystal.dir.path());
    start_fixer(&crystal, &repo, &bin);

    eventually("the distiller's entry is kept", || {
        crystal
            .ok(&["memory", "-C", repo_dir])
            .contains("The ledger tests need redis up  (ledger.rs)")
    });
    // It read what the task was and did, and kept to its own kinds.
    let message = std::fs::read_to_string(repo.join("distill-message")).unwrap();
    assert!(
        message
            .starts_with("The task: fix the ledger\nHow it ended: done: Fixed: start redis first."),
        "{message}"
    );
    assert!(message.contains("USER: fix the ledger\n"), "{message}");
    assert!(
        message.contains("ASSISTANT: Found it: redis was down.\n"),
        "{message}"
    );
    assert!(
        message.contains("TOOL Bash: cargo test\nRESULT: test result: ok\n"),
        "{message}"
    );
    let listed = crystal.ok(&["memory", "-C", repo_dir]);
    assert!(!listed.contains("fixed it"), "{listed}");
    let args = written(&repo.join("distill-args"));
    assert!(args.contains("--model\nclaude-haiku-4-5\n"), "{args}");
    assert!(args.contains("--tools\n\n"), "{args}");
    assert!(args.contains("--no-session-persistence\n"), "{args}");

    // Run again by hand, it finds the same, and says so.
    let said = crystal.ok(&["memory", "distill", "fixer"]);
    assert_eq!(
        said,
        "distilled fixer: 0 entries added, 1 seen again, 1 rejected ($0.0100)\n  \
         rejected entry 2: \"outcome\" isn't a kind it may give\n"
    );
    let events = crystal.ok(&["events", "-k", "memory.distilled"]);
    assert!(
        events.contains("0 added, 1 seen again, 1 rejected ($0.0100)"),
        "{events}"
    );
    let found = crystal.ok(&["memory", "-C", repo_dir, "search", "redis"]);
    assert_eq!(found.lines().count(), 1, "{found}");

    // Forgotten, it stays forgotten.
    let id = found
        .lines()
        .find(|line| line.contains("ledger tests"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    crystal.ok(&["memory", "-C", repo_dir, "rm", &id]);
    let said = crystal.ok(&["memory", "distill", "fixer"]);
    assert!(said.contains("1 forgotten before"), "{said}");
    let listed = crystal.ok(&["memory", "-C", repo_dir]);
    assert!(!listed.contains("ledger tests"), "{listed}");
}

#[test]
fn a_session_with_nothing_to_read_can_t_be_distilled() {
    let (crystal, _repo) = crystal_remembering();
    crystal.ok(&["new", "-n", "plain", "sleep", "30"]);
    let refused = crystal.fails(&["memory", "distill", "plain"]);
    assert!(
        refused.contains("left nothing the distiller can read"),
        "{refused}"
    );
}

#[test]
fn archiving_a_claude_session_has_the_distiller_read_what_it_did() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    std::fs::write(repo.join("ledger.rs"), "fn ledger() {}").unwrap();
    let bin = distilling_claude(crystal.dir.path());
    let out = crystal
        .command(&["new", "-d", "-n", "plain", "-c", repo_dir, "claude"])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Its hooks name its conversation's transcript, which says what it did.
    let args = written(&repo.join("task-args"));
    let settings = claude_settings(&args);
    let hook = settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let transcript = crystal.dir.path().join("conv-9.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"user\",\"message\":{\"content\":\"why do the ledger tests fail?\"}}\n",
    )
    .unwrap();
    let event = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "conv-9",
        "transcript_path": transcript,
    });
    run_hook(&crystal, "plain", hook, &event.to_string());
    eventually("the conversation is known", || {
        crystal.saved().contains("conv-9")
    });

    crystal.ok(&["archive", "plain"]);
    eventually("the distiller's entry is kept", || {
        crystal
            .ok(&["memory", "-C", repo_dir])
            .contains("The ledger tests need redis up  (ledger.rs)")
    });
    let message = std::fs::read_to_string(repo.join("distill-message")).unwrap();
    assert!(
        message.starts_with("The work of session plain, which wasn't started with a task."),
        "{message}"
    );
    assert!(
        message.contains("USER: why do the ledger tests fail?\n"),
        "{message}"
    );
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
    assert!(!args.contains("--mcp-config"), "{args}");
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
    tui.type_keys("jjjjj ");
    tui.shows("● github");
    eventually("gh is asked about pull requests", || {
        std::fs::read_to_string(&calls).is_ok_and(|calls| calls.contains("pr list"))
    });
    let config = std::fs::read_to_string(crystal.config_file()).unwrap();
    assert!(config.contains("github = true"), "{config}");
}

#[test]
fn the_settings_view_changes_the_config_and_follows_it_live() {
    let crystal = Crystal::new();
    crystal.configure("notify = false\n\n[memory]\nembeddings = false\n");
    // The daemon the TUI starts gets a curl that can't download anything,
    // and a cache of the test's own with no model in it.
    let bin = crystal.dir.path().join("curl-bin");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join("curl"),
        "echo 'curl: (6) no network' >&2\nexit 6\n",
    );
    let cache = crystal.dir.path().join("cache");
    let env = [
        ("PATH", path_with(&bin)),
        ("XDG_CACHE_HOME", cache.display().to_string()),
    ];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut tui = crystal.attach_with_env(&[], &env);
    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);
    tui.shows("agent");
    let config = || std::fs::read_to_string(crystal.config_file()).unwrap();

    tui.type_keys(",");
    tui.shows("○ notifications");
    tui.type_keys(" ");
    tui.shows("● notifications");
    assert!(config().starts_with("notify = true\n"), "{}", config());
    tui.shows("● sounds");
    // The theme is the look's.
    tui.type_keys("2l");
    tui.shows("(2 of 20)");
    assert!(config().contains("theme = \"light\""), "{}", config());
    // Back past the first is the last.
    tui.type_keys("h");
    tui.shows("(1 of 20)");
    tui.type_keys("h");
    tui.shows("vesper");
    assert!(config().contains("theme = \"vesper\""), "{}", config());

    // Changed by hand, the file is shown as it is now.
    crystal.configure(
        "notify = true\ntheme = \"light\"\n\n[memory]\ndistill = false\nembeddings = false\n",
    );
    tui.type_keys("6");
    tui.shows("○ distill closed tasks");
    tui.shows("not downloaded (2449 MB)");

    // Turned on, search by meaning has the daemon get the models, and the
    // view follows how that goes: here, a download that fails. It's below
    // the distiller's model and budget.
    tui.type_keys("jjj ");
    tui.shows("● search by meaning");
    assert!(
        config().contains("[memory]\ndistill = false\nembeddings = true\n"),
        "{}",
        config()
    );
    tui.shows("couldn't get them ready");

    // A setting typed in, one up from the last of the sessions' tab, and
    // its default put back.
    tui.type_keys("3Gk\r");
    tui.type_keys("develop\r");
    eventually("the base branch is written down", || {
        config().contains("[worktrees]\nbase = \"develop\"")
    });
    tui.shows("base branch           develop");
    tui.type_keys("\x1b[3~");
    tui.shows("base branch           origin's default");
    assert!(!config().contains("base = "), "{}", config());

    tui.type_keys("\x1b");
    tui.hides("base branch");
    tui.shows("agent");
}

#[test]
fn a_key_pressed_in_the_settings_is_given_its_command_at_once() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);
    let mut tui = crystal.tui();
    tui.shows("❯ agent");
    let config = || std::fs::read_to_string(crystal.config_file()).unwrap_or_default();

    // The keys' tab: the prefix, the key back from a pane, then the
    // sidebar's commands, `commands` the sixth.
    tui.type_keys(",8");
    tui.shows("From a pane");
    tui.type_keys("jjjjjjj\r");
    tui.shows("press a key…");
    tui.type_keys("\x1bOQ");
    eventually("F2 is written down", || {
        config().contains("[keys]\ncommands = \"f2\"")
    });
    tui.shows("• commands              F2");

    // One another command has is taken from it once you say so.
    tui.type_keys("jj\ru");
    tui.shows("u is next-needing-you's: enter takes it from it, esc leaves it");
    tui.type_keys("\r");
    eventually("u moves", || {
        config().contains("needs-you = \"u\"\nnext-needing-you = \"none\"")
    });
    tui.shows("• next-needing-you      none");

    // And it counts straight away: F2 lists the commands.
    tui.type_keys("\x1b");
    tui.hides("From a pane");
    tui.type_keys("\x1bOQ");
    tui.shows("enter runs · ↑/↓ choose · esc closes");
}

#[test]
fn the_keys_of_a_plugin_that_s_off_aren_t_listed() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);
    let mut tui = crystal.tui();
    tui.shows("❯ agent");
    tui.type_keys("?");
    // At 80 by 24, the plugins' sidebar keys are on the second page.
    tui.shows("1/3");
    tui.type_keys(" ");
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
    let settings: serde_json::Value = claude_settings(&args);
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
        [
            "session.started",
            "session.working",
            "session.waiting",
            "session.done"
        ],
        "{heard}"
    );
    let waiting = heard.lines().nth(2).unwrap();
    let json = waiting
        .strip_prefix("session.waiting agent ")
        .unwrap_or_else(|| panic!("{waiting}"));
    let json: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(json["event"], "session.waiting");
    assert_eq!(json["session"]["name"], "agent");
    assert_eq!(json["session"]["activity"], "waiting");
    assert_eq!(json["from"], "working");
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
    // The plugins' keys are on the overlay's second page, after the
    // sidebar's own.
    tui.type_keys("?");
    tui.type_keys(" ");
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
    // Down past crystal's own eight, to the pane under board.
    let open = |tui: &mut Terminal| {
        tui.type_keys("X");
        tui.shows("installed");
        tui.type_keys("jjjjjjjjj\r");
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

/// A session that prints a URL on its first row and a hyperlink, OSC 8,
/// on its second, its text `the docs`.
const LINKS: &str = r"printf 'see https://example.com/docs now\n';
printf '\033]8;;https://example.com/hidden\033\\the docs\033]8;;\033\\\n'; sleep 30";

#[test]
fn ctrl_click_opens_a_link_in_a_pane_and_over_ssh_copies_it() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "linky", "sh", "-c", LINKS]);

    let mut tui = tui_over_ssh(&crystal);
    tui.shows("the docs");
    let (column, row) = (PANE_SCREEN_COLUMN, PANE_SCREEN_ROW);
    // Held over a link with Ctrl, the mouse underlines all of it.
    tui.type_keys(&mouse_move(column + 10, row, true));
    eventually("the link is underlined", || {
        tui.underlined(column as u16 + 4, row as u16)
            && tui.underlined(column as u16 + 27, row as u16)
    });
    assert!(!tui.underlined(column as u16 + 28, row as u16));
    tui.type_keys(&mouse_move(column + 10, row, false));
    eventually("the underline goes", || {
        !tui.underlined(column as u16 + 10, row as u16)
    });

    tui.type_keys(&ctrl_click(column + 10, row));
    tui.copies("https://example.com/docs");
    tui.shows("copied https://example.com/docs");
    // The hyperlink's text isn't the link: where it goes is.
    tui.type_keys(&ctrl_click(column + 2, row + 1));
    tui.copies("https://example.com/hidden");

    // Copy mode's o opens the link under its cursor.
    tui.type_keys("v");
    tui.shows("copying from linky");
    tui.type_keys("?example.com/docs\r");
    tui.shows("1 of 1");
    tui.type_keys("o");
    let copied = base64(b"https://example.com/docs");
    eventually("it's copied a second time", || {
        let written = tui.written.lock().unwrap();
        String::from_utf8_lossy(&written).matches(&copied).count() == 2
    });
    tui.hides("copying from linky");
}

#[test]
fn a_plugin_s_link_handler_opens_the_links_it_takes() {
    let crystal = Crystal::new();
    let manifest = r#"
name = "linker"
version = "1.0.0"

[[actions]]
id = "issue"
title = "Open the issue"
command = ["sh", "issue.sh"]

[[link_handlers]]
pattern = "^https://example\\.com/issues/[0-9]+$"
action = "issue"
"#;
    let script = r#"echo "$CRYSTAL_LINK|$CRYSTAL_SESSION|$CRYSTAL_PLUGIN|$CRYSTAL_PLUGIN_CONFIG_DIR" > opened"#;
    let dir = plugin(&crystal, "linker", manifest, &[("issue.sh", script)]);
    let printing = "echo 'https://example.com/issues/42 or https://e.com/x'; sleep 30";
    crystal.ok(&["new", "-n", "linky", "sh", "-c", printing]);

    let mut tui = tui_over_ssh(&crystal);
    tui.shows("issues/42");
    let issue = ctrl_click(PANE_SCREEN_COLUMN + 5, PANE_SCREEN_ROW);
    // Off, the plugin takes no links.
    tui.type_keys(&issue);
    tui.copies("https://example.com/issues/42");
    assert!(!dir.join("opened").exists());

    crystal.ok(&["plugin", "enable", "linker"]);
    tui.type_keys(&issue);
    let opened = written(&dir.join("opened"));
    let config_dir = crystal.config_home().join("crystal/plugin-config/linker");
    assert_eq!(
        opened,
        format!(
            "https://example.com/issues/42|linky|linker|{}\n",
            config_dir.display()
        )
    );
    tui.shows("ran linker: Open the issue");
    // A link it doesn't take goes to the browser, or here the clipboard.
    tui.type_keys(&ctrl_click(PANE_SCREEN_COLUMN + 36, PANE_SCREEN_ROW));
    tui.copies("https://e.com/x");

    // The plugins view lists what it takes.
    tui.type_keys("X");
    tui.shows(r"link ^https://example\.com/issues/[0-9]+$  → Open the issue");

    // And a link can be tried on it from the command line.
    crystal.ok(&[
        "plugin",
        "run",
        "linker",
        "--link",
        "https://example.com/issues/7",
    ]);
    let opened = written(&dir.join("opened"));
    assert!(
        opened.starts_with("https://example.com/issues/7||linker|"),
        "{opened}"
    );
    let refused = crystal.fails(&["plugin", "run", "linker", "--link", "https://e.com/x"]);
    assert!(
        refused.contains("linker has no link handler that takes https://e.com/x"),
        "{refused}"
    );
}

#[test]
fn a_plugin_builds_as_it_installs_and_a_failed_build_keeps_it_off() {
    let crystal = Crystal::new();
    let source = crystal.dir.path().join("builder-source");
    std::fs::create_dir_all(&source).unwrap();
    let elsewhere = if cfg!(target_os = "macos") {
        "linux"
    } else {
        "macos"
    };
    let manifest = format!(
        r#"name = "builder"
version = "1.0.0"

[[build]]
command = ["sh", "build.sh"]

[[build]]
command = ["sh", "-c", "echo there > elsewhere"]
platforms = ["{elsewhere}"]
"#
    );
    std::fs::write(source.join("plugin.toml"), manifest).unwrap();
    std::fs::write(source.join("build.sh"), "echo compiling\necho ok > built\n").unwrap();
    let source_arg = source.to_str().unwrap();

    let said = crystal.ok(&["plugin", "install", source_arg, "--yes"]);
    assert!(said.contains("build  sh build.sh"), "{said}");
    assert!(said.contains("building builder: sh build.sh\n"), "{said}");
    let installed = crystal.config_home().join("crystal/plugins/builder");
    assert_eq!(written(&installed.join("built")), "ok\n");
    // A build command for another system doesn't run here.
    assert!(!installed.join("elsewhere").exists());
    assert!(
        crystal
            .ok(&["plugin", "log", "builder"])
            .contains("compiling")
    );
    assert!(
        crystal
            .config_home()
            .join("crystal/plugin-config/builder")
            .is_dir()
    );

    // A build that fails turns it off, and keeps it off.
    crystal.ok(&["plugin", "enable", "builder"]);
    std::fs::write(installed.join("build.sh"), "echo 'no libfoo' >&2\nexit 2\n").unwrap();
    let failed = crystal.fails(&["plugin", "build", "builder"]);
    assert!(
        failed.contains("its build failed: sh build.sh ended with exit status: 2\n  no libfoo"),
        "{failed}"
    );
    assert!(failed.contains("so builder is off now"), "{failed}");
    let listed = crystal.ok(&["plugin"]);
    assert!(listed.contains("builder        unbuilt"), "{listed}");
    let refused = crystal.fails(&["plugin", "enable", "builder"]);
    assert!(
        refused.contains("builder can't be turned on: its build failed"),
        "{refused}"
    );

    std::fs::write(installed.join("build.sh"), "echo ok > built\n").unwrap();
    assert_eq!(
        crystal.ok(&["plugin", "build", "builder"]),
        "building builder: sh build.sh\nbuilt builder\n"
    );
    crystal.ok(&["plugin", "enable", "builder"]);

    // One whose build fails as it installs stays, but off.
    crystal.ok(&["plugin", "remove", "builder"]);
    std::fs::write(source.join("build.sh"), "exit 1\n").unwrap();
    let failed = crystal.fails(&["plugin", "install", source_arg, "--yes", "--enable"]);
    assert!(failed.contains("installed builder in"), "{failed}");
    assert!(
        failed.contains("but it's off: its build failed"),
        "{failed}"
    );
    assert!(
        failed.contains("`crystal plugin build builder`"),
        "{failed}"
    );
    assert!(crystal.ok(&["plugin"]).contains("builder        unbuilt"));
}

#[test]
fn a_plugin_s_startup_commands_run_each_time_the_daemon_starts() {
    let crystal = Crystal::new();
    let manifest = r#"
name = "starter"
version = "1.0.0"

[[startup]]
command = ["sh", "start.sh"]
"#;
    let script = r#"echo "$CRYSTAL_EVENT $CRYSTAL_PLUGIN_STATE_DIR $(cat)" >> started"#;
    let dir = plugin(&crystal, "starter", manifest, &[("start.sh", script)]);
    crystal.ok(&["plugin", "enable", "starter"]);

    crystal.ok(&["new", "-n", "first", "sleep", "30"]);
    let state = crystal.socket.with_extension("plugins").join("starter");
    assert_eq!(
        written(&dir.join("started")),
        format!("startup {} \n", state.display())
    );
    assert!(state.is_dir());
    // Once a start, not once a session; and again in a daemon handed over
    // to, its sessions carrying on.
    crystal.ok(&["new", "-n", "second", "sleep", "30"]);
    let pid = crystal.pid("first");
    crystal.ok(&["restart-server"]);
    eventually("it runs again", || {
        lines_in(&dir.join("started")).len() == 2
    });
    assert_eq!(crystal.pid("first"), pid);
    crystal.ok(&["ls"]);
    assert_eq!(lines_in(&dir.join("started")).len(), 2);
}

#[test]
fn a_plugin_for_a_newer_crystal_is_listed_but_can_t_be_turned_on() {
    let crystal = Crystal::new();
    let manifest = "name = \"future\"\nversion = \"1.0.0\"\nmin_crystal_version = \"99.0\"\n";
    let dir = plugin(&crystal, "future", manifest, &[]);
    let listed = crystal.ok(&["plugin"]);
    let why = format!("needs crystal 99.0 or later; this is {OUR_VERSION}");
    assert!(
        listed.contains(&format!("future         unsupported  1.0.0     {why}")),
        "{listed}"
    );
    assert!(listed.contains("handoff        on"), "{listed}");
    let refused = crystal.fails(&["plugin", "enable", "future"]);
    assert!(
        refused.contains(&format!("future can't be turned on: {why}")),
        "{refused}"
    );

    // The plugins view says why too, under crystal's own, and won't
    // switch it on.
    let mut tui = crystal.tui();
    tui.type_keys("X");
    tui.shows("● handoff");
    tui.shows(&format!("○ future          {why}"));
    // Down past crystal's own eight, to future.
    tui.type_keys("jjjjjjjj ");
    tui.shows(&format!("future can't be turned on: {why}"));
    tui.type_keys("\x1b");
    drop(tui);

    // Nor is one installed.
    let source = crystal.dir.path().join("future-source");
    std::fs::rename(&dir, &source).unwrap();
    let refused = crystal.fails(&["plugin", "install", source.to_str().unwrap(), "--yes"]);
    assert!(
        refused.contains(&format!("future can't be installed: it {why}")),
        "{refused}"
    );
}

#[test]
fn plugin_events_lists_every_event_a_hook_can_hear() {
    let crystal = Crystal::new();
    let listed = crystal.ok(&["plugin", "events"]);
    assert!(listed.starts_with("EVENT"), "{listed}");
    for (event, when) in [
        ("session.waiting", "a session's agent comes to wait on you"),
        (
            "session.unarchived",
            "a session is started again from the archive",
        ),
        ("run.tool_use", "a background task's Claude uses a tool"),
        (
            "memory.distill_failed",
            "the distiller couldn't read what a session did",
        ),
    ] {
        let line = listed
            .lines()
            .find(|line| line.starts_with(&format!("{event} ")))
            .unwrap_or_else(|| panic!("{event}: {listed}"));
        assert!(line.ends_with(when), "{line}");
    }
}

#[test]
fn plugin_run_json_hands_a_hook_the_event_it_gives() {
    let crystal = Crystal::new();
    let manifest = r#"
name = "listener"
version = "1"

[[events]]
on = "*"
command = ["sh", "hook.sh"]
"#;
    let hook = r#"{ printf '%s|%s|%s|' "$CRYSTAL_EVENT" "$CRYSTAL_EVENT_TEXT" "$CRYSTAL_SESSION"; cat; } > heard"#;
    let dir = plugin(&crystal, "listener", manifest, &[("hook.sh", hook)]);
    let heard = || std::fs::read_to_string(dir.join("heard")).unwrap();

    let closed = r#"{"event":"task.closed","task":{"goal":"Ship it","outcome":{"failed":true,"summary":"tests red","closed":1}}}"#;
    crystal.ok(&["plugin", "run", "listener", "--json", closed]);
    let said = heard();
    assert!(
        said.starts_with("task.closed|example: failed: tests red|example|{"),
        "{said}"
    );
    let json: serde_json::Value =
        serde_json::from_str(said.splitn(4, '|').nth(3).unwrap()).unwrap();
    assert_eq!(json["task"]["goal"], "Ship it");
    // What it didn't give, the made-up event has.
    assert_eq!(json["task"]["id"], 12);

    // --event names it, and the JSON comes on standard input, like a line
    // of `crystal events --json`.
    let mut run = crystal.command(&[
        "plugin",
        "run",
        "listener",
        "--event",
        "session.waiting",
        "--json",
        "-",
    ]);
    let mut child = run.stdin(Stdio::piped()).spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"event":"session.done","session":{"name":"docs"}}"#)
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert!(
        heard().starts_with("session.waiting|docs: working → waiting|docs|"),
        "{}",
        heard()
    );

    let refused = crystal.fails(&["plugin", "run", "listener", "--json", "[1]"]);
    assert!(refused.contains("--json is an object"), "{refused}");
    let refused = crystal.fails(&["plugin", "run", "listener", "--json", "{}"]);
    assert!(refused.contains("say which event"), "{refused}");
    let refused = crystal.fails(&[
        "plugin",
        "run",
        "listener",
        "--json",
        r#"{"event":"task.*"}"#,
    ]);
    assert!(refused.contains("say one event"), "{refused}");
}

#[test]
fn a_hook_runs_as_long_as_its_plugin_says() {
    let crystal = Crystal::new();
    let manifest = r#"
name = "slow"
version = "1"
timeout_secs = 1

[[events]]
on = "session.started"
command = ["sh", "-c", "sleep 5"]
"#;
    plugin(&crystal, "slow", manifest, &[]);
    crystal.ok(&["plugin", "enable", "slow"]);
    crystal.ok(&["new", "-d", "-n", "agent", "sleep", "30"]);
    eventually("the hook is stopped", || {
        crystal
            .ok(&["plugin", "log", "slow"])
            .contains("still running after 1s, so it was stopped")
    });
}

/// `example`, one of the example plugins, put in `dir`.
fn example_plugin(example: &str, dir: &Path) -> PathBuf {
    let from = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/plugins")
        .join(example);
    let to = dir.join(example);
    std::fs::create_dir_all(&to).unwrap();
    for entry in std::fs::read_dir(&from).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
    to
}

#[test]
fn a_projects_plugin_runs_once_it_s_on_for_it_and_hears_only_its_project() {
    let crystal = Crystal::new();
    let app = git_repo(crystal.dir.path(), "app");
    let other = git_repo(crystal.dir.path(), "other");
    let (app_arg, other_arg) = (app.to_str().unwrap(), other.to_str().unwrap());
    example_plugin("worktree-env", &app.join(".crystal/plugins"));
    for repo in [&app, &other] {
        std::fs::write(repo.join(".env"), "SECRET=1\n").unwrap();
    }

    let plugins = crystal.ok(&["plugin", "-C", app_arg]);
    assert!(plugins.contains("app's own, in "), "{plugins}");
    assert!(plugins.contains("worktree-env  off"), "{plugins}");
    assert!(
        !crystal
            .ok(&["plugin", "-C", other_arg])
            .contains("worktree-env")
    );
    // Not the user's own, nor on until it's turned on for the project.
    let refused = crystal.fails(&["plugin", "enable", "worktree-env"]);
    assert!(
        refused.contains("there's no plugin called worktree-env"),
        "{refused}"
    );
    let refused = crystal.fails(&[
        "plugin",
        "enable",
        "worktree-env",
        "--project",
        "-C",
        app_arg,
    ]);
    assert!(refused.contains("--yes"), "{refused}");

    let said = crystal.ok(&[
        "plugin",
        "enable",
        "worktree-env",
        "--project",
        "-C",
        app_arg,
        "--yes",
    ]);
    assert!(said.contains("on worktree.created  sh copy.sh"), "{said}");
    assert!(
        said.ends_with("the worktree-env plugin is on, for app alone\n"),
        "{said}"
    );
    let config = std::fs::read_to_string(crystal.config_file()).unwrap();
    assert!(config.contains("[[project]]\npath = "), "{config}");
    assert!(config.contains("plugins = [\"worktree-env\"]"), "{config}");
    assert!(
        crystal
            .ok(&["plugin", "-C", app_arg])
            .contains("worktree-env  on")
    );

    // The other project's worktree comes first: by the time app's has its
    // .env, the plugin would have heard of the other's.
    crystal.ok(&[
        "new",
        "-d",
        "-n",
        "elsewhere",
        "-c",
        other_arg,
        "-w",
        "elsewhere",
        "sleep",
        "30",
    ]);
    crystal.ok(&[
        "new", "-d", "-n", "feature", "-c", app_arg, "-w", "feature", "sleep", "30",
    ]);
    let worktree =
        |name: &str| PathBuf::from(listed(&crystal, name)["cwd"].as_str().unwrap().to_string());
    let copied = worktree("feature").join(".env");
    eventually("the .env is copied", || copied.is_file());
    assert_eq!(std::fs::read_to_string(&copied).unwrap(), "SECRET=1\n");
    assert!(!worktree("elsewhere").join(".env").exists());
    let log = crystal.ok(&["plugin", "log", "worktree-env", "--project", "-C", app_arg]);
    assert!(log.contains("copied .env into"), "{log}");

    crystal.ok(&[
        "plugin",
        "disable",
        "worktree-env",
        "--project",
        "-C",
        app_arg,
    ]);
    let config = std::fs::read_to_string(crystal.config_file()).unwrap();
    assert!(!config.contains("[[project]]"), "{config}");
}

#[test]
fn the_example_plugins_do_what_they_say() {
    let crystal = Crystal::new();
    let plugins = crystal.config_home().join("crystal/plugins");
    for example in ["event-log", "slack"] {
        let source = example_plugin(example, &crystal.dir.path().join("examples"));
        crystal.ok(&[
            "plugin",
            "install",
            source.to_str().unwrap(),
            "--yes",
            "--enable",
        ]);
    }
    assert!(plugins.join("slack/post.sh").is_file());

    // event-log keeps each event, and its action empties the log.
    crystal.ok(&["new", "-d", "-n", "agent", "sleep", "30"]);
    let state = crystal
        .dir
        .path()
        .join("crystal.plugins/event-log/events.jsonl");
    eventually("the event is logged", || {
        std::fs::read_to_string(&state)
            .is_ok_and(|log| log.contains(r#""event":"session.started""#))
    });
    crystal.ok(&["plugin", "run", "event-log", "clear"]);
    assert_eq!(std::fs::read_to_string(&state).unwrap(), "");

    // slack posts what failed, with the URL it's given, and nothing else.
    let bin = crystal.dir.path().join("curl-bin");
    std::fs::create_dir_all(&bin).unwrap();
    script(&bin.join("curl"), "printf '%s\\n' \"$@\" > \"$CURL_LOG\"\n");
    let curl_log = crystal.dir.path().join("curl.log");
    let post = |json: &str| {
        let out = crystal
            .command(&["plugin", "run", "slack", "--json", json])
            .env("PATH", path_with(&bin))
            .env("CURL_LOG", &curl_log)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let failed = r#"{"event":"task.closed","task":{"outcome":{"failed":true,"summary":"tests \"red\"","closed":1}}}"#;
    assert!(post(failed).contains("no webhook yet"));
    assert!(!curl_log.exists());
    let config = crystal.config_home().join("crystal/plugin-config/slack");
    std::fs::write(config.join("webhook-url"), "https://hooks.example/T1\n").unwrap();
    post(failed);
    let sent = std::fs::read_to_string(&curl_log).unwrap();
    assert!(
        sent.contains(r#"{"text":"[task.closed] example: failed: tests \"red\""}"#),
        "{sent}"
    );
    assert!(sent.ends_with("https://hooks.example/T1\n"), "{sent}");
    std::fs::remove_file(&curl_log).unwrap();
    post(
        r#"{"event":"task.closed","task":{"outcome":{"failed":false,"summary":"fixed","closed":1}}}"#,
    );
    assert!(!curl_log.exists(), "a task done isn't posted");
}

#[test]
fn a_plugin_pane_opens_where_it_says_from_the_command_line() {
    let crystal = Crystal::new();
    let manifest = r#"
name = "board"
version = "0.1.0"

[[panes]]
id = "show"
title = "The board"
command = ["sh", "show.sh"]
"#;
    let script = "echo \"the board for $CRYSTAL_PLUGIN\"\nexec sleep 30\n";
    plugin(&crystal, "board", manifest, &[("show.sh", script)]);
    crystal.ok(&["new", "-d", "-n", "agent", "sleep", "30"]);
    let refused = crystal.fails(&["plugin", "pane", "open", "board", "show"]);
    assert!(
        refused.contains("turn it on with `crystal plugin enable board`"),
        "{refused}"
    );
    crystal.ok(&["plugin", "enable", "board"]);

    // Over the panes takes a TUI; its session goes with the try.
    let refused = crystal.fails(&["plugin", "pane", "open", "board", "show"]);
    assert!(refused.contains("no TUI is running"), "{refused}");
    assert!(
        refused.contains("split, zoomed or tab do without one"),
        "{refused}"
    );
    assert!(crystal.row("board-show").is_none());
    let refused = crystal.fails(&[
        "plugin",
        "pane",
        "open",
        "board",
        "show",
        "--placement",
        "split",
        "--width",
        "40",
    ]);
    assert!(refused.contains("only a popup has a width"), "{refused}");
    let refused = crystal.fails(&["plugin", "pane", "open", "board", "nope"]);
    assert!(
        refused.contains("board has no pane nope; its panes: show"),
        "{refused}"
    );

    // Split off beside a session, or in a tab of its own, it's a session
    // among the others, TUI or not.
    let said = crystal.ok(&[
        "plugin",
        "pane",
        "open",
        "board",
        "show",
        "--placement",
        "split",
        "--session",
        "agent",
        "--down",
    ]);
    assert_eq!(said, "board-show\n");
    let layout = crystal.ok(&["layout"]);
    assert!(
        layout.contains(
            "one above the other, 50% first\n      the selection's: agent\n      board-show\n"
        ),
        "{layout}"
    );
    crystal.ok(&[
        "plugin",
        "pane",
        "open",
        "board",
        "show",
        "--placement",
        "tab",
    ]);
    let layout = crystal.ok(&["layout"]);
    assert!(
        layout.contains("2 The board (in front)\n  sessions  board-show-2\n"),
        "{layout}"
    );

    // With a TUI, a popup comes up over everything, with the keyboard.
    crystal.ok(&["kill", "board-show"]);
    crystal.ok(&["kill", "board-show-2"]);
    crystal.ok(&["tab", "close", "2"]);
    let mut tui = crystal.tui();
    tui.shows("agent");
    let said = crystal.ok(&[
        "plugin",
        "pane",
        "open",
        "board",
        "show",
        "--placement",
        "popup",
        "--height",
        "10",
        "--session",
        "agent",
    ]);
    assert_eq!(said, "board-show\n");
    tui.shows("board · The board");
    tui.shows("the board for board");
    let refused = crystal.fails(&["plugin", "pane", "open", "board", "show"]);
    assert!(
        refused.contains("The board is open over the panes already"),
        "{refused}"
    );
    tui.type_keys("\x1c");
    tui.hides("board · The board");
    eventually("its session ends with it", || {
        crystal.row("board-show").is_none()
    });
}

#[test]
fn a_plugin_pane_placed_in_a_split_opens_beside_the_selected_session_in_the_tui() {
    let crystal = Crystal::new();
    let manifest = r#"
name = "board"
version = "0.1.0"

[[panes]]
id = "show"
title = "The board"
placement = "split"
command = ["sh", "show.sh"]
"#;
    plugin(
        &crystal,
        "board",
        manifest,
        &[("show.sh", "echo 'the board says hi'\nexec sleep 30\n")],
    );
    crystal.ok(&["plugin", "enable", "board"]);
    crystal.ok(&["new", "-d", "-n", "agent", "sleep", "30"]);
    let mut tui = crystal.tui();
    tui.shows("agent");
    // Down past crystal's own eight, to the pane under board.
    tui.type_keys("X");
    tui.shows("installed");
    tui.type_keys("jjjjjjjjj\r");
    tui.shows("the board says hi");
    tui.hides("crystal's own");
    let layout = crystal.ok(&["layout"]);
    assert!(layout.contains("side by side, 50% first\n"), "{layout}");
    assert!(layout.contains("board-show"), "{layout}");
    // A session among the others, it stays once its program has ended.
    assert!(crystal.row("board-show").is_some());
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
/// in `dir`, whatever directory a step runs in. Each prompt it reads notes
/// its arguments and the prompt in `runs`, a line each (`<args> --
/// <prompt>|`, the prompt's lines joined by `|`), and it answers at once:
/// `answer <n>` for the n-th prompt, in a conversation of its own,
/// `conv-<n>` for the prompt it started on, or the one it was told to
/// resume. A prompt with `FAIL` in it fails, until the test makes `fixed`;
/// one with `SLOW` in it waits for the test to make `go` before it answers.
fn flow_claude(dir: &Path) -> PathBuf {
    let bin = dir.join("flow-bin");
    std::fs::create_dir(&bin).unwrap();
    let claude = bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
cd '{dir}'
conversation=""
previous=""
for arg; do
    if [ "$previous" = "--resume" ]; then conversation="$arg"; fi
    previous="$arg"
done
turns=0
while IFS= read -r line; do
    case "$line" in *'"type":"user"'*) ;; *) continue ;; esac
    prompt=$(printf '%s\n' "$line" | sed 's/.*"content":"\([^"]*\)".*/\1/; s/\\n/|/g')
    echo "$* -- $prompt|" >> runs
    run=$(wc -l < runs | tr -d ' ')
    turns=$((turns + 1))
    if [ -z "$conversation" ]; then conversation="conv-$run"; fi
    echo '{{"type":"system","subtype":"init","session_id":"'"$conversation"'","cwd":"/x","model":"m"}}'
    case "$prompt" in
    *SLOW*) while [ ! -e go ]; do sleep 0.05; done ;;
    esac
    case "$prompt" in
    *FAIL*) if [ ! -e fixed ]; then
        echo '{{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"'"$conversation"'","total_cost_usd":0.01,"duration_ms":10}}'
        continue
    fi ;;
    esac
    cost=$(awk "BEGIN {{ print $turns * 0.5 }}")
    echo '{{"type":"result","subtype":"success","is_error":false,"result":"answer '"$run"'","session_id":"'"$conversation"'","total_cost_usd":'"$cost"',"duration_ms":10}}'
done
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
    eventually("the plan step's session is waiting", || {
        status(&crystal, "gated-1-plan") == "waiting"
    });
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
    // Build again, as a follow-up on its own claude, told the notes and
    // what the review said.
    let build = &runs[3];
    assert!(
        build.contains(" -- Build add retries") && !build.contains("--resume"),
        "{build}"
    );
    assert!(
        build.contains("sent back at the review step, with these notes: keep the old default"),
        "{build}"
    );
    assert!(build.contains("What review said:|answer 3"), "{build}");
    // Then the review again, on its own claude, of the new build.
    assert!(
        runs[4].contains(" -- Review answer 4") && !runs[4].contains("--resume"),
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
        runs[1].contains(&format!(
            "--resume conv-1 --allowedTools {ALLOWED} -- Plan FAIL"
        )),
        "{}",
        runs[1]
    );
    // The step's session reads idle once its turn has been seen, a moment
    // after the flow is done.
    eventually("the plan step's session is idle", || {
        status(&crystal, "pair-1-plan") == "idle"
    });
}

#[test]
fn a_step_cut_short_by_a_restart_is_interrupted_until_it_runs_again() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let path = path_with(&flow_claude(dir));
    // A daemon of the test's own, to crash, that finds the fake claude:
    // after a restart, steps start from the daemon's environment.
    let start_daemon = || crystal.start_daemon_with(&[("PATH", &path)]);
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
        runs[1].contains(&format!(
            "--resume conv-1 --allowedTools {ALLOWED} -- Plan SLOW down"
        )),
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
    // On a new branch with a made-up name, an adjective and an animal, as
    // the new-session panel gives a new worktree, whatever the goal says.
    let build = crystal.row("tree-1-build").unwrap();
    let branch = build[4].clone();
    let (adjective, animal) = branch.split_once('-').unwrap();
    assert!(
        [adjective, animal]
            .iter()
            .all(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase())),
        "{branch}"
    );
    for step in ["tree-1-build", "tree-1-check"] {
        let row = crystal.row(step).unwrap();
        assert_eq!(row[4], branch);
        let dir = format!("app.worktrees/{branch}");
        assert!(row[5].ends_with(&dir), "{}", row[5]);
    }

    // A second run of the same goal makes a worktree of its own.
    let out = flow_ok(
        &crystal,
        &path,
        &["tree", "add retries", "-c", &repo_arg, "--wait"],
    );
    assert_eq!(out, "tree-2\ndone\n");
    let second = crystal.row("tree-2-build").unwrap();
    assert_ne!(second[5], build[5]);
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
    assert!(runs[1].contains(" -- Plan add retries"), "{}", runs[1]);
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

#[test]
fn cancelling_a_flow_cancels_its_steps_task_and_it_goes_no_further() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let path = path_with(&flow_claude(dir));
    assert_eq!(
        flow_ok(&crystal, &path, &["pair", "SLOW", "down"]),
        "pair-1\n"
    );
    runs(dir, 1);

    crystal.ok(&["flow", "cancel", "pair-1"]);
    let err = crystal.fails(&["flow", "wait", "pair-1"]);
    assert!(err.contains("cancelled at plan"), "{err}");
    let show = crystal.ok(&["flow", "show", "pair-1"]);
    assert!(show.contains("pair-1 · pair · cancelled at plan"), "{show}");
    eventually("the step's task is cancelled", || {
        let tasks = crystal.ok(&["tasks"]);
        tasks.contains("cancelled") && tasks.contains("pair-1 plan: SLOW down")
    });
    assert_ne!(status(&crystal, "pair-1-plan"), "working");

    let again = crystal.fails(&["flow", "cancel", "pair-1"]);
    assert!(again.contains("pair-1 is cancelled already"), "{again}");
    let retried = crystal.fails(&["flow", "retry", "pair-1"]);
    assert!(
        retried.contains("only a failed or interrupted step"),
        "{retried}"
    );
    let ended = events(&crystal, &["-k", "flow.ended"]);
    assert_eq!(ended[0]["flow"]["state"], "cancelled");
    assert_eq!(ended[0]["flow"]["step"], "plan");
}

#[test]
fn a_projects_own_flows_run_and_defs_says_where_each_is_written() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "app");
    let repo_arg = repo.to_str().unwrap();
    std::fs::create_dir(repo.join(".crystal")).unwrap();
    std::fs::write(
        repo.join(".crystal/flows.toml"),
        "[[flow]]\nname = \"pair\"\n\n[[flow.step]]\nname = \"only\"\n\
         prompt = \"Just {goal} as {slug}, round {round}\"\n",
    )
    .unwrap();

    let defs = crystal.ok(&["flow", "defs", "-C", repo_arg]);
    let line = |name: &str| {
        let found = defs
            .lines()
            .find(|line| line.starts_with(&format!("{name} ")));
        found
            .unwrap_or_else(|| panic!("no {name} in {defs}"))
            .to_string()
    };
    assert!(defs.starts_with("FLOW "), "{defs}");
    let pair = line("pair");
    assert!(
        pair.contains(" only ") && pair.ends_with("app/.crystal/flows.toml"),
        "{defs}"
    );
    let ship = line("ship");
    assert!(
        ship.contains("plan → build → review") && ship.ends_with("crystal/config.toml"),
        "{defs}"
    );

    let path = path_with(&flow_claude(dir));
    let out = flow_ok(
        &crystal,
        &path,
        &["pair", "add retries", "-c", repo_arg, "--wait"],
    );
    assert_eq!(out, "pair-1\ndone\n");
    let runs = runs(dir, 1);
    assert!(
        runs[0].ends_with(" -- Just add retries as add-retries, round 1|"),
        "{}",
        runs[0]
    );
}

/// A stand-in for Codex in a flow's step: it writes down what it was
/// asked, closes its task with `crystal done` as it was told to, and waits.
fn finishing_codex(dir: &Path) -> PathBuf {
    let bin = dir.join("codex-bin");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join("codex"),
        &format!(
            "printf '%s\\n' \"$@\" > codex-args.new && mv codex-args.new codex-args\n\
             {CRYSTAL} done \"built it in a terminal\"\n\
             sleep 30\n"
        ),
    );
    bin
}

#[test]
fn a_step_on_another_agent_runs_in_a_terminal_and_ends_as_its_task_closes() {
    let crystal = Crystal::new();
    crystal.configure(
        r#"
notify = false

[plugins]
memory = false

[[profile]]
name = "coder"
agent = "codex"

[[flow]]
name = "mixed"

[[flow.step]]
name = "plan"
prompt = "Plan {goal}"

[[flow.step]]
name = "build"
profile = "coder"
prompt = "Build {goal} following {plan.summary}"
"#,
    );
    let dir = crystal.dir.path();
    let path = format!(
        "{}:{}",
        finishing_codex(dir).display(),
        path_with(&flow_claude(dir))
    );
    let out = flow_ok(&crystal, &path, &["mixed", "add retries", "--wait"]);
    assert_eq!(out, "mixed-1\ndone\n");

    // Codex was asked its step's prompt, and told how to close its task in
    // its developer instructions.
    let args = written(&dir.join("codex-args"));
    assert!(
        args.ends_with("\n--\nBuild add retries following answer 1\n"),
        "{args}"
    );
    assert!(
        args.starts_with("-c\ndeveloper_instructions=") && args.contains("crystal done"),
        "{args}"
    );
    let listed: serde_json::Value = serde_json::from_str(&crystal.ok(&["flow", "--json"])).unwrap();
    assert_eq!(listed[0]["steps"][1]["answer"], "built it in a terminal");
    assert_eq!(listed[0]["steps"][1]["session"], "mixed-1-build");
    let tasks = crystal.ok(&["tasks"]);
    assert!(
        tasks.contains("mixed-1 build: add retries — built it in a terminal"),
        "{tasks}"
    );
}

#[test]
fn a_step_sets_its_own_agent_and_model_runs_claude_in_a_terminal_and_carries_criteria() {
    let crystal = Crystal::new();
    crystal.configure(
        r#"
notify = false

[plugins]
memory = false

[[flow]]
name = "own"

[[flow.step]]
name = "plan"
background = false
model = "opus"
accept = ["names the files", "says the order"]
prompt = "Plan {goal}"

[[flow.step]]
name = "build"
agent = "codex"
model = "gpt-5"
prompt = "Build {goal} following {plan.summary}"
"#,
    );
    let dir = crystal.dir.path();
    // A Claude Code in a terminal that closes its task as Codex does.
    let bin = dir.join("claude-bin");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join("claude"),
        &format!(
            "printf '%s\\n' \"$@\" > claude-args.new && mv claude-args.new claude-args\n\
             {CRYSTAL} done \"planned it in a terminal\"\n\
             sleep 30\n"
        ),
    );
    let path = format!("{}:{}", bin.display(), path_with(&finishing_codex(dir)));
    let out = flow_ok(&crystal, &path, &["own", "add retries", "--wait"]);
    assert_eq!(out, "own-1\ndone\n");

    // Claude Code ran in a terminal, on the step's model, asked its prompt
    // with the criteria under it, and told how to close its task.
    let args = written(&dir.join("claude-args"));
    assert!(args.contains("\n--model\nopus\n--\n"), "{args}");
    assert!(
        args.ends_with(
            "\n--\nPlan add retries\n\nAcceptance criteria:\n- names the files\n- says the order\n"
        ),
        "{args}"
    );
    assert!(args.contains("crystal done"), "{args}");
    let card = crystal.ok(&["tasks", "show", "own-1-plan"]);
    assert!(
        card.contains("  accept    names the files\n            says the order\n"),
        "{card}"
    );
    // Codex, with no profile, on the step's model.
    let args = written(&dir.join("codex-args"));
    assert!(args.contains("-m\ngpt-5\n"), "{args}");
    assert!(
        args.ends_with("\n--\nBuild add retries following planned it in a terminal\n"),
        "{args}"
    );
}

#[test]
fn a_step_in_a_terminal_carries_on_through_a_handover() {
    let crystal = Crystal::new();
    crystal.configure(
        r#"
notify = false

[plugins]
memory = false

[[profile]]
name = "coder"
agent = "codex"

[[flow]]
name = "mixed"

[[flow.step]]
name = "plan"
prompt = "Plan {goal}"

[[flow.step]]
name = "build"
profile = "coder"
prompt = "Build {goal}"
"#,
    );
    let dir = crystal.dir.path();
    // A Codex that waits for the test before it closes its task.
    let bin = dir.join("codex-bin");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join("codex"),
        &format!(
            "echo > codex-started
             while [ ! -e go ]; do sleep 0.05; done
             {CRYSTAL} done \"built it after the handover\"
             sleep 30
"
        ),
    );
    let path = format!("{}:{}", bin.display(), path_with(&flow_claude(dir)));
    assert_eq!(
        flow_ok(&crystal, &path, &["mixed", "add retries"]),
        "mixed-1\n"
    );
    written(&dir.join("codex-started"));
    let pid = crystal.pid("mixed-1-build");

    crystal.ok(&["restart-server"]);
    assert_eq!(crystal.pid("mixed-1-build"), pid);
    std::fs::write(dir.join("go"), "").unwrap();
    assert_eq!(flow_waits(&crystal, "mixed-1"), "done\n");
    let listed: serde_json::Value = serde_json::from_str(&crystal.ok(&["flow", "--json"])).unwrap();
    assert_eq!(
        listed[0]["steps"][1]["answer"],
        "built it after the handover"
    );
}

/// The events `crystal events --json` prints with `args`, each one parsed.
fn events(crystal: &Crystal, args: &[&str]) -> Vec<serde_json::Value> {
    let mut all = vec!["events", "--json"];
    all.extend(args);
    crystal
        .ok(&all)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The names of `events`, in order.
fn names(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["event"].as_str().unwrap().to_string())
        .collect()
}

/// Reads what a command prints, a line at a time, on a thread of its own,
/// so a test can wait for the next line without hanging on it forever.
fn lines_of(child: &mut std::process::Child) -> std::sync::mpsc::Receiver<String> {
    use std::io::BufRead;
    let stdout = child.stdout.take().unwrap();
    let (lines, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

fn next_line(lines: &std::sync::mpsc::Receiver<String>) -> String {
    lines
        .recv_timeout(Duration::from_secs(5))
        .expect("a line within 5s")
}

#[test]
fn the_event_log_keeps_what_happened_to_a_session_through_its_renames() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-d", "-n", "brief", "sh", "-c", "read go; exit 4"]);
    crystal.ok(&["rename", "brief", "memo"]);
    crystal.ok(&["send", "memo", "go"]);
    assert_eq!(crystal.ok(&["wait", "memo"]), "exited 4\n");
    // The daemon tells of an end as it next looks over its sessions.
    eventually("its end is in the log", || {
        crystal
            .ok(&["events", "-n", "memo"])
            .contains("session.ended")
    });

    let about_memo = events(&crystal, &["-n", "memo"]);
    assert_eq!(
        names(&about_memo),
        [
            "session.started",
            "session.renamed",
            "session.message",
            "session.ended"
        ]
    );
    let seqs: Vec<u64> = about_memo
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect();
    assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]), "{seqs:?}");
    assert_eq!(about_memo[1]["from"], "brief");
    assert_eq!(about_memo[2]["message"]["line"], "go");
    assert_eq!(about_memo[3]["session"]["status"], "exited 4");

    crystal.ok(&["kill", "memo"]);
    // Gone, it's known by the names it had.
    assert_eq!(
        names(&events(&crystal, &["-n", "brief"])),
        ["session.started"]
    );
    assert_eq!(
        names(&events(&crystal, &["-n", "memo", "-k", "session.removed"])),
        ["session.removed"]
    );
    assert!(events(&crystal, &["--since", "0s"]).is_empty());

    let printed = crystal.ok(&["events", "-k", "session.renamed"]);
    assert!(
        printed.contains("session.renamed     memo  was brief"),
        "{printed}"
    );
    let err = crystal.fails(&["events", "-k", "sesion.*"]);
    assert!(err.contains("matches no event"), "{err}");
    let err = crystal.fails(&["events", "--since", "yesterday"]);
    assert!(err.contains("nor a time"), "{err}");
}

#[test]
fn events_follow_catches_up_from_the_log_then_streams_what_happens() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-d", "-n", "first", "sleep", "30"]);
    let follow = |args: &[&str]| {
        let mut all = vec!["events", "--follow", "--json", "-k", "session.started"];
        all.extend(args);
        let mut child = crystal
            .command(&all)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let lines = lines_of(&mut child);
        (child, lines)
    };
    let parse = |line: String| -> serde_json::Value { serde_json::from_str(&line).unwrap() };
    let (mut caught_up, caught_up_lines) = follow(&["--since", "1h"]);
    let first = parse(next_line(&caught_up_lines));
    assert_eq!(first["session"]["name"], "first");

    // Without --since, it hears only what happens once it's listening.
    let (mut new_only, new_only_lines) = follow(&[]);
    let heard = (0..25).find_map(|n| {
        crystal.ok(&["new", "-d", "-n", &format!("probe-{n}"), "sleep", "30"]);
        new_only_lines.recv_timeout(Duration::from_millis(200)).ok()
    });
    let heard = parse(heard.expect("the follower heard a session start"));
    assert!(
        heard["session"]["name"]
            .as_str()
            .unwrap()
            .starts_with("probe-")
    );

    let next = parse(next_line(&caught_up_lines));
    assert_eq!(next["session"]["name"], "probe-0");
    assert!(next["seq"].as_u64() > first["seq"].as_u64());
    caught_up.kill().unwrap();
    new_only.kill().unwrap();
}

#[test]
fn wait_until_catches_a_state_however_short_and_fails_when_the_program_ends_first() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);
    let hook = format!("'{CRYSTAL}' hook claude");
    let waiting_for = |until: &str| {
        crystal
            .command(&["wait", "agent", "--until", until])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let printed = |child: std::process::Child| {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };

    // Working only a moment, between two reports, still counts. However
    // long the wait takes to start listening, a turn after that is heard.
    let mut working = waiting_for("working");
    eventually("the wait has seen it working", || {
        run_hook(
            &crystal,
            "agent",
            &hook,
            r#"{"hook_event_name":"UserPromptSubmit"}"#,
        );
        run_hook(&crystal, "agent", &hook, r#"{"hook_event_name":"Stop"}"#);
        working.try_wait().unwrap().is_some()
    });
    assert_eq!(printed(working), "working\n");
    assert_eq!(
        crystal.ok(&["wait", "agent", "--until", "waiting,done"]),
        "done\n"
    );

    let err = crystal.fails(&["wait", "agent", "--until", "waiting", "--timeout", "0.3"]);
    assert!(err.contains("agent wasn't waiting after 0.3s"), "{err}");

    crystal.ok(&["new", "-n", "brief", "sh", "-c", "read go; exit 4"]);
    let waiting = crystal
        .command(&["wait", "brief", "--until", "waiting"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // It ends only once it's sent a line, and fails the wait either way.
    crystal.ok(&["send", "brief", "go"]);
    let out = waiting.wait_with_output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("brief ended (exited 4) without being waiting"),
        "{err}"
    );
    assert_eq!(
        crystal.ok(&["wait", "brief", "--until", "exited"]),
        "exited 4\n"
    );
}

#[test]
fn wait_output_returns_the_line_that_matches_once_it_shows() {
    let crystal = Crystal::new();
    let script = "echo booting; read go; echo ready on port 4000; sleep 30";
    crystal.ok(&["new", "-n", "server", "sh", "-c", script]);
    shows_on_screen(&crystal, "server", "booting");

    let waiting = crystal
        .command(&["wait", "server", "--output", "port [0-9]+"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Before the line or after it, the wait ends on it.
    crystal.ok(&["send", "server", "go"]);
    let out = waiting.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "ready on port 4000\n"
    );

    // What's on screen already counts.
    assert_eq!(
        crystal.ok(&["wait", "server", "--output", "boot"]),
        "booting\n"
    );
    let err = crystal.fails(&["wait", "server", "--output", "crashed", "--timeout", "0.3"]);
    assert!(
        err.contains("nothing on server's screen matched `crashed` after 0.3s"),
        "{err}"
    );
    let err = crystal.fails(&["wait", "server", "--output", "("]);
    assert!(err.contains("isn't a regular expression"), "{err}");

    crystal.ok(&["new", "-n", "quiet", "sh", "-c", "read go"]);
    let waiting = crystal
        .command(&["wait", "quiet", "--output", "never"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    crystal.ok(&["send", "quiet", "go"]);
    let out = waiting.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("quiet has ended, and nothing on its screen matches"),
        "{err}"
    );
}

#[test]
fn a_background_task_s_runs_and_its_task_are_in_the_log() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    finish_run(dir, 1);
    let out = crystal
        .command(&["task", "--wait", "-n", "fixer", "fix the tests"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    eventually("the task has closed", || {
        crystal
            .ok(&["events", "-k", "task.closed"])
            .contains("fixer")
    });

    let logged = events(&crystal, &["-n", "fixer"]);
    assert_eq!(
        names(&logged),
        [
            "session.started",
            "task.opened",
            "run.started",
            "session.working",
            "run.tool_use",
            "run.ended",
            "session.done",
            "task.closed"
        ]
    );
    assert_eq!(logged[2]["run"]["prompt"], "fix the tests");
    assert_eq!(logged[4]["run"]["tool"]["name"], "Bash");
    assert_eq!(logged[4]["run"]["tool"]["gist"], "cargo test");
    assert_eq!(logged[5]["run"]["cost_usd"], 0.0421);
    assert_eq!(logged[5]["run"]["answer"], "All green on run 1.");
    assert_eq!(logged[7]["task"]["outcome"]["failed"], false);
}

#[test]
fn the_backlog_and_memory_tell_the_log_what_changed() {
    let (crystal, repo) = crystal_remembering();
    let repo_dir = repo.to_str().unwrap();
    // Memory is kept by whoever's asked; the daemon has to be there to hear.
    crystal.ok(&["new", "-d", "-n", "here", "sleep", "30"]);
    crystal.ok(&["backlog", "-C", repo_dir, "add", "Retry", "the", "webhook"]);
    crystal.ok(&["backlog", "-C", repo_dir, "done", "1"]);
    crystal.ok(&[
        "remember",
        "-C",
        repo_dir,
        "The ledger tests need the database",
    ]);
    crystal.ok(&["memory", "-C", repo_dir, "rm", "1"]);

    let logged = events(&crystal, &["-C", repo_dir]);
    assert_eq!(
        names(&logged),
        [
            "backlog.added",
            "backlog.closed",
            "memory.added",
            "memory.forgotten"
        ]
    );
    assert_eq!(logged[0]["backlog"]["text"], "Retry the webhook");
    assert_eq!(logged[1]["backlog"]["done"], true);
    assert_eq!(
        logged[2]["memory"]["text"],
        "The ledger tests need the database"
    );
    let printed = crystal.ok(&["events", "-k", "memory.*"]);
    assert!(
        printed.contains("1 (note) The ledger tests need the database"),
        "{printed}"
    );
}

#[test]
fn a_flow_s_steps_and_gate_are_in_the_log() {
    let crystal = Crystal::new();
    crystal.configure(FLOWS);
    let dir = crystal.dir.path();
    let path = path_with(&flow_claude(dir));
    let out = flow_ok(&crystal, &path, &["gated", "add retries", "--wait"]);
    assert_eq!(out, "gated-1\nwaiting at plan\n");
    crystal.ok(&["flow", "approve", "gated-1"]);
    assert_eq!(flow_waits(&crystal, "gated-1"), "done\n");

    let logged = events(&crystal, &["-k", "flow.*"]);
    assert_eq!(
        names(&logged),
        [
            "flow.started",
            "flow.step_started",
            "flow.step_ended",
            "flow.gate",
            "flow.gate_answered",
            "flow.step_started",
            "flow.step_ended",
            "flow.ended"
        ]
    );
    let flow = |n: usize| logged[n]["flow"].clone();
    assert_eq!(flow(0)["run"], "gated-1");
    assert_eq!(flow(1)["step"], "plan");
    assert_eq!(logged[1]["session"]["name"], "gated-1-plan");
    assert_eq!(flow(2)["state"], "waiting");
    assert_eq!(flow(2)["said"], "answer 1");
    assert_eq!(flow(4)["state"], "approved");
    assert_eq!(flow(7)["state"], "done");
    assert_eq!(flow(7)["cost_usd"], 1.0);
}

#[test]
fn plugin_run_event_tries_a_plugin_s_hooks_on_a_made_up_event() {
    let crystal = Crystal::new();
    let manifest = r#"
name = "listener"
version = "1.0.0"

[[events]]
on = "task.*"
command = ["sh", "hook.sh"]
"#;
    let hook = r#"printf '%s ' "$CRYSTAL_EVENT"; cat; [ -z "$FAIL" ]"#;
    plugin(&crystal, "listener", manifest, &[("hook.sh", hook)]);

    // Off, it's tried all the same.
    let out = crystal.ok(&["plugin", "run", "listener", "--event", "task.closed"]);
    let json = out
        .strip_prefix("task.closed ")
        .unwrap_or_else(|| panic!("{out}"));
    let event: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(event["event"], "task.closed");
    assert_eq!(event["task"]["outcome"]["failed"], false);
    assert_eq!(event["session"]["name"], "example");

    let out = crystal
        .command(&["plugin", "run", "listener", "--event", "task.opened"])
        .env("FAIL", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = crystal.fails(&["plugin", "run", "listener", "--event", "session.done"]);
    assert!(
        err.contains("listener has no hook on session.done"),
        "{err}"
    );
    let err = crystal.fails(&["plugin", "run", "listener", "--event", "task.*"]);
    assert!(err.contains("say one event"), "{err}");
}

#[test]
fn an_event_plugin_hears_what_happens_beyond_sessions() {
    let crystal = Crystal::new();
    let manifest = r#"
name = "listener"
version = "1.0.0"

[[events]]
on = "backlog.added"
command = ["sh", "hook.sh"]
"#;
    let hook = r#"{ printf '%s ' "$CRYSTAL_EVENT"; cat; } >> heard"#;
    let dir = plugin(&crystal, "listener", manifest, &[("hook.sh", hook)]);
    crystal.ok(&["plugin", "enable", "listener"]);
    // The first request the daemon answers: its hooks hear it all the same.
    crystal.ok(&["backlog", "add", "Retry", "the", "webhook"]);
    // The hook writes the event's name, then its JSON, which the daemon
    // ends with a newline.
    let heard = written(&dir.join("heard"));
    let json = heard
        .strip_prefix("backlog.added ")
        .unwrap_or_else(|| panic!("{heard}"));
    let event: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(event["backlog"]["text"], "Retry the webhook");
    assert!(event["seq"].as_u64().unwrap() > 0);
}

/// A stand-in for an agent crystal doesn't know, `pi`, that says what it's
/// doing itself with `crystal report`: working as it starts, then a stage
/// at a time, as the test makes each stage's file in its directory, idle
/// with the command that resumes it, waiting on the user, and letting go of
/// its session. A `quit` file has it quit at once, without letting go. It
/// writes down the arguments it was started with. Returns the directory to
/// put on the PATH.
fn fake_reporting_agent(dir: &Path) -> PathBuf {
    let bin = dir.join("pi-bin");
    std::fs::create_dir(&bin).unwrap();
    let body = format!(
        r#"
crystal='{CRYSTAL}'
wait_for() {{
    while [ ! -e "$1" ]; do [ -e quit ] && exit 0; sleep 0.05; done
}}
printf '%s\n' "$@" > pi-args.new && mv pi-args.new pi-args
"$crystal" report working --agent pi
wait_for rest; "$crystal" report idle -- pi --resume 's 1'
wait_for ask; "$crystal" report waiting -m 'approve the deploy'
wait_for leave; "$crystal" report --release
wait_for quit
"#
    );
    script(&bin.join("pi"), &body);
    bin
}

impl Crystal {
    /// Makes the file that moves [`fake_reporting_agent`] on to `stage`.
    fn stage(&self, stage: &str) {
        std::fs::write(self.dir.path().join(stage), "").unwrap();
    }

    /// The session called `name`, as `ls --json` lists it.
    fn listed(&self, name: &str) -> serde_json::Value {
        let listed: Vec<serde_json::Value> =
            serde_json::from_str(&self.ok(&["ls", "--json"])).unwrap();
        let found = listed.into_iter().find(|session| session["name"] == name);
        found.unwrap_or_else(|| panic!("there's no session called {name}"))
    }
}

#[test]
fn an_agent_says_what_it_does_and_holds_its_session_until_it_lets_go() {
    let crystal = Crystal::new();
    let bin = fake_reporting_agent(crystal.dir.path());
    let out = crystal
        .command(&["new", "-d", "-n", "agent", "pi"])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = || crystal.row("agent").unwrap()[1].clone();
    eventually("it says it's working", || status() == "working");
    let listed = crystal.listed("agent");
    assert_eq!(listed["front"]["kind"], "agent");
    assert_eq!(listed["front"]["program"], "pi");

    crystal.stage("rest");
    eventually("it's done with its turn", || status() == "done");
    crystal.stage("ask");
    eventually("it waits on the user", || status() == "waiting");
    let reporter = crystal.listed("agent")["reporter"].clone();
    assert_eq!(reporter["agent"], "pi");
    assert_eq!(reporter["message"], "approve the deploy");
    assert_eq!(
        reporter["resume"],
        serde_json::json!(["pi", "--resume", "s 1"])
    );

    crystal.stage("leave");
    eventually("it has let go", || status() == "running");
    assert!(crystal.listed("agent")["reporter"].is_null());
    let told = events(&crystal, &["-n", "agent", "-k", "session.*"]);
    assert_eq!(
        names(&told),
        [
            "session.started",
            "session.claimed",
            "session.working",
            "session.done",
            "session.waiting",
            "session.released"
        ]
    );
    let printed = crystal.ok(&["events", "-n", "agent", "-k", "session.waiting"]);
    assert!(
        printed.contains("done → waiting: approve the deploy"),
        "{printed}"
    );
}

#[test]
fn a_report_that_cant_be_taken_says_why() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-d", "-n", "agent", "sleep", "30"]);
    let report = |args: &[&str]| {
        let mut all = vec!["report", "-n", "agent"];
        all.extend(args);
        crystal.fails(&all)
    };
    let err = report(&["--session-only", "--", "pi", "--resume", "1"]);
    assert!(err.contains("no agent reports for agent yet"), "{err}");
    let err = report(&["idle", "--", "/usr/local/bin/pi"]);
    assert!(err.contains("has to start with a command's name"), "{err}");
    let err = report(&["idle", "--", "pi", "it's"]);
    assert!(err.contains("has a quote in it"), "{err}");
    let err = report(&["working", "--agent", "two words"]);
    assert!(err.contains("one word"), "{err}");
    assert_eq!(crystal.row("agent").unwrap()[1], "running");

    let out = crystal
        .command(&["report", "working"])
        .env_remove("CRYSTAL_SESSION_ID")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("isn't running in a crystal session"), "{err}");
}

#[test]
fn a_line_and_a_model_reported_for_a_session_show_on_its_row() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-d", "-n", "indexer", "sleep", "30"]);
    crystal.ok(&[
        "report",
        "-n",
        "indexer",
        "--line",
        "indexing 40%",
        "--model",
        "pi-large",
    ]);
    let listed = crystal.listed("indexer");
    assert_eq!(listed["line"], "indexing 40%", "{listed}");
    assert_eq!(listed["model"], "pi-large", "{listed}");
    // For the sidebar alone: the status is still crystal's to read.
    assert_eq!(listed["status"], "running", "{listed}");
    assert!(listed["reporter"].is_null(), "{listed}");
    let tui = crystal.tui();
    tui.shows("indexing 40%");
    tui.shows("indexer pi-large");

    // A report numbered lower than the last from its source came late.
    let numbered = |seq: &str, line: &str| {
        crystal.ok(&[
            "report", "-n", "indexer", "--source", "ci", "--seq", seq, "--line", line,
        ]);
    };
    numbered("2", "the second report");
    numbered("1", "the first report");
    assert_eq!(crystal.listed("indexer")["line"], "the second report");
    tui.shows("the second report");

    // A line given a while goes once it's up; an empty one takes it off.
    crystal.ok(&["report", "-n", "indexer", "--line", "brief", "--ttl", "1s"]);
    assert_eq!(crystal.listed("indexer")["line"], "brief");
    eventually("the line's time is up", || {
        crystal.listed("indexer")["line"].is_null()
    });
    tui.hides("brief");
    crystal.ok(&["report", "-n", "indexer", "--model", ""]);
    assert!(crystal.listed("indexer")["model"].is_null());

    let err = crystal.fails(&["report", "-n", "indexer", "--line", "x", "--ttl", "25h"]);
    assert!(err.contains("from a second to a day"), "{err}");
    let err = crystal.fails(&["report", "-n", "indexer", "--line", "x", "--source", "a b"]);
    assert!(err.contains("letters, digits"), "{err}");
    crystal.fails(&["report", "-n", "indexer", "--ttl", "5s"]);
}

#[test]
fn an_agent_s_model_follows_its_hooks_and_a_switch_in_its_transcript() {
    let crystal = Crystal::new();
    let bin = fake_claude(crystal.dir.path());
    let out = crystal
        .command(&["new", "-n", "agent", "claude", "--model", "opus"])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(out.status.success());
    // Until anything else says, the model its command gave it.
    eventually("the command's model shows", || {
        crystal.listed("agent")["model"] == "opus"
    });
    let args = written(&crystal.dir.path().join("args"));
    let settings = claude_settings(&args);
    let hook = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();

    // Its hooks say the model it started on, and where its conversation is.
    let transcript = crystal.dir.path().join("conv-1.jsonl");
    let started = serde_json::json!({
        "hook_event_name": "SessionStart",
        "source": "startup",
        "session_id": "conv-1",
        "transcript_path": transcript,
        "model": "claude-opus-5-5",
    });
    run_hook(&crystal, "agent", hook, &started.to_string());
    assert_eq!(crystal.listed("agent")["model"], "claude-opus-5-5");

    // `/model` fires no hook: the switch is read from the conversation.
    let switched = r#"{"type":"user","message":{"content":"<local-command-stdout>Set model to `Sonnet 5` and saved as your default</local-command-stdout>"}}"#;
    std::fs::write(&transcript, format!("{switched}\n")).unwrap();
    eventually("the switch shows", || {
        crystal.listed("agent")["model"] == "Sonnet 5"
    });
    let tui = crystal.tui();
    tui.shows("agent sonnet 5");
}

#[test]
fn an_agent_that_reports_for_itself_holds_its_session_through_a_handover() {
    let crystal = Crystal::new();
    let bin = fake_reporting_agent(crystal.dir.path());
    let out = crystal
        .command(&["new", "-d", "-n", "agent", "pi"])
        .env("PATH", path_of(&[&bin]))
        .output()
        .unwrap();
    assert!(out.status.success());
    let status = || crystal.row("agent").unwrap()[1].clone();
    crystal.stage("rest");
    eventually("it's done with its turn", || status() == "done");
    let pid = crystal.pid("agent");

    crystal.ok(&["restart-server"]);
    assert_eq!(crystal.pid("agent"), pid);
    assert_eq!(status(), "done");
    let reporter = crystal.listed("agent")["reporter"].clone();
    assert_eq!(reporter["agent"], "pi");
    assert_eq!(
        reporter["resume"],
        serde_json::json!(["pi", "--resume", "s 1"])
    );
    // It goes on saying what it's doing, to the daemon handed over to.
    crystal.stage("ask");
    eventually("it waits on the user", || status() == "waiting");
    assert_eq!(
        crystal.listed("agent")["reporter"]["message"],
        "approve the deploy"
    );
    let told = events(&crystal, &["-n", "agent", "-k", "session.released"]);
    assert!(told.is_empty(), "it never let go: {told:?}");
}

#[test]
fn an_agent_comes_back_after_a_restart_with_the_command_it_said_resumes_it() {
    let crystal = Crystal::new();
    let bin = fake_reporting_agent(crystal.dir.path());
    let path = path_of(&[&bin]);
    let args = crystal.dir.path().join("pi-args");
    crystal.stage("rest");
    let daemon = crystal.start_daemon();
    let out = crystal
        .command(&["new", "-d", "-n", "agent", "pi", "--fresh"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(written(&args), "--fresh\n");
    eventually("its resume command is saved", || {
        crystal
            .saved()
            .contains(r#""resume":["pi","--resume","s 1"]"#)
    });

    crash(daemon);
    std::fs::remove_file(&args).unwrap();
    let out = crystal
        .command(&["new", "-d", "-n", "other", "sleep", "300"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    // Run in place of its command, which is still the one it was started
    // with, it says what it's doing again.
    assert_eq!(written(&args), "--resume\ns 1\n");
    assert_eq!(crystal.row("agent").unwrap()[7], "pi --fresh");
    eventually("it holds its session again", || {
        crystal.listed("agent")["reporter"]["agent"] == "pi"
    });

    // With the config saying not to, it starts as it was asked to at first.
    crystal.configure(
        "notify = false\nname_from_prompt = false\nresume_reported_agents = false\n\n\
         [plugins]\nmemory = false\n",
    );
    std::fs::remove_file(&args).unwrap();
    let out = crystal
        .command(&["restart-server", "--cold"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(written(&args), "--fresh\n");
}

#[test]
fn an_agent_in_a_shell_is_typed_back_in_after_a_restart_and_lets_go_as_it_leaves() {
    let crystal = Crystal::new();
    let bin = fake_reporting_agent(crystal.dir.path());
    let path = path_of(&[&bin]);
    let args = crystal.dir.path().join("pi-args");
    crystal.stage("rest");
    let daemon = crystal.start_daemon();
    let out = crystal
        .command(&["new", "-d", "-n", "box", "sh"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    crystal.ok(&["send", "box", "pi"]);
    eventually("its resume command is saved", || {
        crystal
            .saved()
            .contains(r#""resume":["pi","--resume","s 1"]"#)
    });

    crash(daemon);
    std::fs::remove_file(&args).unwrap();
    let out = crystal
        .command(&["new", "-d", "-n", "other", "sleep", "300"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    // The shell comes back, and has the command typed into it.
    assert_eq!(written(&args), "--resume\ns 1\n");
    assert_eq!(crystal.row("box").unwrap()[7], "sh");

    // Gone without letting go, it lets go as the shell is back in front.
    crystal.stage("quit");
    eventually("the shell is back, holding no agent", || {
        let listed = crystal.listed("box");
        listed["reporter"].is_null() && listed["front"]["kind"] == "shell"
    });
}

#[test]
fn a_session_crystal_named_takes_its_name_from_its_first_prompt() {
    let crystal = Crystal::new();
    crystal.configure("notify = false\nname_from_prompt = true\n\n[plugins]\nmemory = false\n");
    let bin = fake_claude(crystal.dir.path());
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let new = |args: &[&str]| {
        let out = crystal.command(args).env("PATH", &path).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    // Asked something as it starts, it's named for that at once.
    let fix = ["new", "-d", "claude", "Fix the login redirect"];
    assert_eq!(new(&fix), "fix-login-redirect\n");
    assert_eq!(new(&fix), "fix-login-redirect-2\n");

    // Asked nothing, it's named after its program until its first prompt.
    assert_eq!(new(&["new", "-d", "claude"]), "claude\n");
    let args = written(&crystal.dir.path().join("args"));
    let settings: serde_json::Value = claude_settings(&args);
    let hook = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let prompt = |session: &str, text: &str| {
        let event = serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": text});
        let id = crystal.listed(session)["id"].as_str().unwrap().to_string();
        let env = [("CRYSTAL_SESSION", session), ("CRYSTAL_SESSION_ID", &id)];
        run_hook_with(&crystal, &env, hook, &event.to_string());
    };
    prompt("claude", "/clear");
    assert!(
        crystal.row("claude").is_some(),
        "a slash command names nothing"
    );
    prompt("claude", "Review the diff on this branch");
    assert!(crystal.row("claude").is_none());
    assert_eq!(
        crystal.row("review-diff-branch").unwrap()[1],
        "working",
        "the prompt counts as ever"
    );
    // Only its first.
    prompt("review-diff-branch", "Now the tests");
    assert!(crystal.row("review-diff-branch").is_some());
    let renamed = events(&crystal, &["-k", "session.renamed"]);
    assert_eq!(renamed.len(), 1);
    assert_eq!(renamed[0]["from"], "claude");
    assert_eq!(renamed[0]["session"]["name"], "review-diff-branch");

    // A name given stays, and so does one a script has typed into the
    // session by.
    new(&["new", "-d", "-n", "mine", "claude"]);
    prompt("mine", "Write the docs");
    assert!(crystal.row("mine").is_some());
    assert_eq!(new(&["new", "-d", "claude"]), "claude\n");
    crystal.ok(&["send", "claude", "hello"]);
    prompt("claude", "Write the docs");
    assert!(crystal.row("claude").is_some());
}

#[test]
fn the_timeline_shows_what_happens_as_it_happens_and_enter_goes_to_the_session() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-d", "-n", "worker", "sleep", "30"]);
    crystal.ok(&["new", "-d", "-n", "other", "sleep", "30"]);
    let mut tui = crystal.tui();
    tui.shows("❯ other");
    tui.type_keys("a");
    tui.shows("session.started");
    tui.shows("worker");

    // While it's open, what happens comes in on top.
    crystal.ok(&["rename", "worker", "builder"]);
    tui.shows("was worker");
    tui.type_keys("renamed");
    tui.hides("session.started");

    // Enter goes to the session the line is about, by the name it has now.
    tui.type_keys("\r");
    tui.hides("session.renamed");
    eventually("builder is selected", || {
        let text = tui.text();
        text.lines()
            .last()
            .is_some_and(|footer| footer.contains("builder"))
    });
}

#[test]
fn a_sessions_own_timeline_widens_with_ctrl_s_and_capital_m_reads_what_it_left() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let repo = git_repo(dir, "app");
    let repo_arg = repo.to_str().unwrap();
    let plan = repo.join("plan.md");
    std::fs::write(&plan, "# The plan\n\nPort the codec first.\n").unwrap();
    crystal.ok(&[
        "new",
        "-d",
        "-n",
        "planner",
        "-c",
        repo_arg,
        "-t",
        "write the plan",
        "sleep",
        "30",
    ]);
    crystal.ok(&["handoff", "-n", "planner", "the fixtures live in tests"]);
    crystal.ok(&[
        "done",
        "-n",
        "planner",
        "--artifact",
        plan.to_str().unwrap(),
        "wrote it",
    ]);
    crystal.ok(&["new", "-d", "-n", "bystander", "sleep", "30"]);
    crystal.ok(&["rename", "bystander", "onlooker"]);

    let mut tui = crystal.tui();
    tui.type_keys("/planner");
    tui.shows("1 match");
    tui.type_keys("\r");
    tui.shows("❯ planner");

    // Its own timeline: what happened to it, and nobody else's.
    tui.type_keys("I");
    tui.shows("timeline of session planner");
    tui.shows("task.artifact");
    tui.shows("kept plan.md");
    assert!(!tui.text().contains("was bystander"), "{}", tui.text());
    tui.type_keys("\x13");
    tui.shows("timeline of task t1");
    tui.shows("handoff.added");
    tui.type_keys("\x13");
    tui.shows("timeline of project app");
    tui.type_keys("\x13");
    tui.hides("timeline of");
    tui.shows("was bystander");
    // While it's open, what happens to it comes in on top, and only that.
    tui.type_keys("\x13");
    tui.shows("timeline of session planner");
    crystal.ok(&["rename", "onlooker", "watcher"]);
    crystal.ok(&["rename", "planner", "writer"]);
    tui.shows("was planner");
    assert!(!tui.text().contains("was onlooker"), "{}", tui.text());
    tui.type_keys("\x1b");
    tui.hides("timeline of");
    tui.shows("❯ writer");

    // What it left: the worktree's notes, then what its task kept, each
    // read beside the list.
    tui.type_keys("M");
    tui.shows("handoff · writer · t1");
    tui.shows("notes now");
    tui.shows("the fixtures live in tests");
    tui.shows("kept · 34 bytes");
    tui.shows("notes as t1 closed");
    tui.type_keys("j");
    tui.shows("Port the codec first.");
    tui.type_keys("\x1b");
    tui.hides("notes now");
}

#[test]
fn the_timeline_stays_live_through_a_handover() {
    let crystal = Crystal::new();
    crystal.ok(&["new", "-d", "-n", "worker", "sleep", "30"]);
    let mut tui = crystal.tui();
    tui.shows("❯ worker");
    tui.type_keys("a");
    tui.shows("session.started");

    crystal.ok(&["restart-server"]);
    // It picks the log up again after the last event it showed: the
    // handover itself, then what happens after.
    tui.shows("daemon.handed_over");
    crystal.ok(&["rename", "worker", "builder"]);
    tui.shows("was worker");
    assert!(!tui.text().contains("stopped following"), "{}", tui.text());
}

#[test]
fn a_permission_is_answered_from_the_list_of_what_needs_you() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let path = path_with(&print_claude(dir));
    crystal.ok(&["new", "-d", "-n", "quiet", "sleep", "30"]);
    start_task(&crystal, &path, "fixer", "ASK first");
    eventually("the task waits on the user", || {
        status(&crystal, "fixer") == "waiting"
    });

    let mut tui = crystal.tui();
    tui.type_keys("U");
    tui.shows("needs you · 1 waiting on you");
    tui.shows("Bash cargo test");
    tui.type_keys("y");
    let answer: serde_json::Value = serde_json::from_str(&answers(dir, 1)[0]).unwrap();
    assert_eq!(answer["response"]["response"]["behavior"], "allow");

    // The list stays open, and follows: answered, the task carries on, and
    // finishes its turn in the pane behind the list, seen.
    tui.shows("nothing needs you");
    eventually("the task is done", || status(&crystal, "fixer") == "idle");
}

#[test]
fn coming_back_the_footer_says_what_happened_while_you_were_away() {
    let crystal = Crystal::new();
    let mut tui = crystal.tui();
    tui.type_keys("q");
    assert!(tui.exit());

    crystal.ok(&["new", "-d", "-n", "fixer", "-t", "tidy up", "sleep", "30"]);
    crystal.ok(&["done", "-n", "fixer", "tidied"]);
    crystal.ok(&[
        "new", "-d", "-n", "breaker", "-t", "break it", "sleep", "30",
    ]);
    crystal.ok(&["done", "-n", "breaker", "--failed", "it", "held"]);

    let mut tui = crystal.tui();
    tui.shows("while you were away: 1 task done · 1 failed");
    // `a` opens the timeline with what came since marked.
    tui.type_keys("a");
    tui.shows("task.closed");
    tui.shows(" • ");
}

/// What a terminal sends for a right click at `(column, row)`.
fn right_click(column: usize, row: usize) -> String {
    let (x, y) = (column + 1, row + 1);
    format!("\x1b[<2;{x};{y}M\x1b[<2;{x};{y}m")
}

#[test]
fn a_right_click_on_a_session_opens_its_menu_and_an_item_does_what_its_key_does() {
    let crystal = Crystal::new();
    for name in ["alpha", "beta"] {
        let script = format!("echo {name} is here; echo > {name}-ready; sleep 30");
        crystal.ok(&["new", "-n", name, "sh", "-c", &script]);
        written(&crystal.dir.path().join(format!("{name}-ready")));
    }

    let mut tui = crystal.tui();
    tui.shows("alpha is here");
    let row = line_with(&tui.text(), "❯ beta");
    tui.type_keys(&right_click(10, row));
    tui.shows("archive it");
    tui.shows("beta is here");
    tui.type_keys("x");
    tui.shows("kill beta? y/n");
    tui.type_keys("y");
    eventually("beta is killed", || crystal.row("beta").is_none());
}

#[test]
fn projects_stay_listed_with_no_session_until_taken_off() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let app = git_repo(dir, "app");
    let app = app.to_str().unwrap();
    crystal.ok(&["new", "-d", "-n", "s", "-c", app, "sleep", "30"]);
    eventually("app is listed", || crystal.ok(&["project"]).contains("app"));
    let refused = crystal.fails(&["project", "rm", app]);
    assert!(refused.contains("has 1 session in it"), "{refused}");

    // With its last session gone, it stays.
    crystal.ok(&["kill", "s"]);
    let listed: serde_json::Value =
        serde_json::from_str(&crystal.ok(&["project", "--json"])).unwrap();
    assert_eq!(listed[0]["name"], "app");
    assert_eq!(listed[0]["branch"], "main");
    assert_eq!(listed[0]["sessions"], 0);

    crystal.ok(&["project", "rm", app]);
    assert!(!crystal.ok(&["project"]).contains("app"));

    let api = git_repo(dir, "api");
    crystal.ok(&["project", "add", api.join(".").to_str().unwrap()]);
    let listed = crystal.ok(&["project"]);
    assert!(
        listed.lines().any(|line| line.starts_with("api ")),
        "{listed}"
    );

    let plain = dir.join("plain");
    std::fs::create_dir(&plain).unwrap();
    let refused = crystal.fails(&["project", "add", plain.to_str().unwrap()]);
    assert!(refused.contains("isn't in a git repository"), "{refused}");
}

#[test]
fn a_projects_run_command_runs_in_a_session_of_its_own_and_open_runs_once() {
    let crystal = Crystal::new();
    let app = git_repo(crystal.dir.path(), "app");
    std::fs::create_dir(app.join(".crystal")).unwrap();
    std::fs::write(
        app.join(".crystal/project.toml"),
        "run = \"echo running > ran; sleep 30\"\nopen = \"pwd > opened\"\n",
    )
    .unwrap();
    let here = app.to_str().unwrap();

    let name = crystal.ok(&["project", "run", "-d", "-C", here]);
    assert_eq!(name, "run-app\n");
    assert_eq!(written(&app.join("ran")), "running\n");
    let again = crystal.fails(&["project", "run", "-d", "-C", here]);
    assert!(again.contains("run-app runs it already"), "{again}");
    crystal.ok(&["project", "run", "--stop", "-C", here]);
    assert!(crystal.row("run-app").is_none());

    crystal.ok(&["project", "open", "-C", here]);
    let opened = written(&app.join("opened"));
    assert_eq!(
        Path::new(opened.trim()).canonicalize().unwrap(),
        app.canonicalize().unwrap()
    );

    // The config's [[project]] takes the place of the project's own file.
    crystal.configure(&format!(
        "notify = false\nname_from_prompt = false\n\n[plugins]\nmemory = false\n\n\
         [[project]]\npath = \"{here}\"\nrun = \"echo configured > ran; sleep 30\"\n"
    ));
    crystal.ok(&["project", "run", "-d", "-C", here]);
    eventually("the configured command ran", || {
        std::fs::read_to_string(app.join("ran")).is_ok_and(|text| text == "configured\n")
    });

    let none = crystal.fails(&[
        "project",
        "open",
        "-C",
        crystal.dir.path().to_str().unwrap(),
    ]);
    assert!(none.contains("isn't in a git repository"), "{none}");
}

#[test]
fn an_archived_agent_leaves_the_list_and_comes_back_where_it_was() {
    let crystal = Crystal::new();
    let bin = fake_reporting_agent(crystal.dir.path());
    let path = path_of(&[&bin]);
    let args = crystal.dir.path().join("pi-args");
    crystal.stage("rest");
    let out = crystal
        .command(&["new", "-d", "-n", "agent", "pi", "--fresh"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(written(&args), "--fresh\n");
    eventually("it said how to resume it", || {
        !crystal.listed("agent")["reporter"]["resume"].is_null()
    });
    let pid = crystal.pid("agent");

    crystal.ok(&["archive", "agent"]);
    assert!(crystal.row("agent").is_none());
    eventually("its program has gone", || !alive(pid));
    let archived = crystal.ok(&["ls", "--archived"]);
    let row = archived.lines().nth(1).unwrap();
    assert!(row.starts_with("agent "), "{archived}");
    assert!(row.contains(" yes "), "it resumes: {archived}");

    std::fs::remove_file(&args).unwrap();
    let out = crystal
        .command(&["unarchive", "-d", "agent"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "agent\n");
    assert_eq!(written(&args), "--resume\ns 1\n");
    assert_eq!(crystal.ok(&["ls", "--archived"]), "");
    let events = crystal.ok(&["events", "-k", "session.unarchived"]);
    assert!(events.contains("back from the archive"), "{events}");

    // Killing an archived session takes it out of the archive.
    crystal.ok(&["archive", "agent"]);
    crystal.ok(&["kill", "agent"]);
    assert_eq!(crystal.ok(&["ls", "--archived"]), "");
    let none = crystal.fails(&["unarchive", "agent"]);
    assert!(
        none.contains("no session named agent in the archive"),
        "{none}"
    );
}

#[test]
fn an_agent_left_idle_is_stopped_and_starts_again_where_it_was() {
    let crystal = Crystal::new();
    crystal.configure(
        "notify = false\nname_from_prompt = false\n\n[plugins]\nmemory = false\n\n\
         [sessions]\nstop_idle_after = \"1s\"\n",
    );
    let bin = fake_reporting_agent(crystal.dir.path());
    let path = path_of(&[&bin]);
    let args = crystal.dir.path().join("pi-args");
    crystal.stage("rest");
    let out = crystal
        .command(&["new", "-d", "-n", "agent", "pi"])
        .env("PATH", &path)
        .env("CRYSTAL_IDLE_CHECK_MS", "100")
        .output()
        .unwrap();
    assert!(out.status.success());
    // A program that isn't an agent is never stopped.
    crystal.ok(&["new", "-d", "-n", "plain", "sleep", "30"]);
    assert_eq!(written(&args), "\n");
    // Its turn ended with nobody watching: it's done, news to the user,
    // and stays until they've seen it.
    eventually("its turn has ended", || {
        crystal.listed("agent")["activity"] == "done"
    });
    thread::sleep(Duration::from_millis(1500));
    assert_eq!(crystal.row("agent").unwrap()[1], "done");
    let mut seen = crystal.attach(&["attach", "agent"]);
    seen.type_keys("\x1c");
    assert!(seen.exit());
    eventually("the idle agent is stopped", || {
        crystal.row("agent").unwrap()[1] == "stopped idle"
    });
    assert_eq!(crystal.row("plain").unwrap()[1], "running");

    std::fs::remove_file(&args).unwrap();
    let out = crystal
        .command(&["respawn", "agent"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(written(&args), "--resume\ns 1\n");
}
#[test]
fn keys_the_config_gives_run_their_commands_and_colon_lists_every_one() {
    let crystal = Crystal::new();
    crystal.configure("[keys]\nkill = \"X\"\nquit = \"none\"\nprefix = \"ctrl+a\"\n");
    let keys = crystal.ok(&["keys"]);
    let line = |id: &str| {
        keys.lines()
            .find(|line| line.split_whitespace().next() == Some(id))
            .unwrap_or_else(|| panic!("no {id} in {keys}"))
            .to_string()
    };
    assert!(line("kill").contains(" X "), "{keys}");
    assert!(line("quit").contains(" - "), "{keys}");
    assert!(line("prefix").contains("ctrl+a"), "{keys}");

    let mut tui = crystal.tui();
    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);
    tui.shows("agent");
    // `q` quits no more, and `X` kills.
    tui.type_keys("q");
    tui.type_keys("X");
    tui.shows("kill agent? y/n");
    tui.type_keys("n");
    tui.hides("kill agent?");

    // From inside the pane, the prefix and `:` open the list of commands.
    tui.type_keys("\r");
    tui.shows("typing into");
    tui.type_keys("\x01");
    tui.shows("ctrl+a …");
    tui.type_keys(":");
    tui.shows("select the session below");
    tui.type_keys("quit");
    tui.shows("quit the TUI");
    // Run from the list, it asks first too.
    tui.type_keys("\r");
    tui.shows("quit crystal?");
    tui.type_keys("y");
    assert!(tui.exit());
}

#[test]
fn a_key_of_the_users_own_opens_a_popup_from_a_pane_or_runs_in_the_background() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    let config = format!(
        r#"
[keys]
prefix = ["ctrl+b", "ctrl+a"]

[[keys.command]]
key = "direct+ctrl+alt+g"
type = "popup"
command = "echo 'the popup says hi'; read -r line; echo \"got $line\" > {got}"
description = "the board"
width = "60%"

[[keys.command]]
key = "ctrl+t"
type = "shell"
command = "echo \"$CRYSTAL_SESSION\" > {ran}"

[[keys.command]]
key = "ctrl+e"
type = "shell"
command = "echo 'it broke' >&2; exit 3"
description = "break"
"#,
        got = dir.join("got").display(),
        ran = dir.join("ran").display(),
    );
    crystal.configure(&config);
    let keys = crystal.ok(&["keys"]);
    assert!(keys.contains("[[keys.command]]"), "{keys}");
    assert!(keys.contains("direct+ctrl+alt+g"), "{keys}");
    assert!(keys.contains("ctrl+b ctrl+a"), "{keys}");

    let mut tui = crystal.tui();
    crystal.ok(&["new", "-n", "agent", "sleep", "30"]);
    tui.shows("agent");
    // A sidebar key runs its command in the background, about the session.
    tui.type_keys("\x14");
    assert_eq!(written(&dir.join("ran")), "agent\n");
    // One that fails says so, with the last of what it said.
    tui.type_keys("\x05");
    tui.shows("break failed (exit status: 3): it broke");

    // From inside the pane, with no prefix, the popup opens with the
    // keyboard; its program ending closes it.
    tui.type_keys("\r");
    tui.shows("typing into");
    tui.type_keys("\x1b\x07");
    tui.shows("the popup says hi");
    tui.shows(" the board ");
    eventually("its session starts", || crystal.row("board").is_some());
    tui.type_keys("hello\r");
    assert_eq!(written(&dir.join("got")), "got hello\n");
    tui.hides("the popup says hi");
    eventually("its session ends with it", || {
        crystal.row("board").is_none()
    });
}

#[test]
fn a_key_the_config_cant_make_sense_of_is_an_error_that_names_it() {
    let crystal = Crystal::new();
    crystal.configure("[keys]\nnew-sesion = \"n\"\n");
    let err = crystal.fails(&["keys"]);
    assert!(
        err.contains("new-sesion") && err.contains("config.toml"),
        "{err}"
    );
}

/// This crystal's version.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What this machine's release is called, as `crystal update` picks it.
fn release_target() -> String {
    let os = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else {
        "unknown-linux-musl"
    };
    format!("{}-{os}", std::env::consts::ARCH)
}

/// crystal's releases, on a web server of the test's own that serves them
/// the way GitHub does: `latest` redirects to the latest release's page,
/// and each release's files are under `download/v<version>/`.
struct Releases {
    dir: TempDir,
    url: String,
    /// How many times it has been asked which release is the latest.
    asked: Arc<Mutex<usize>>,
}

impl Releases {
    /// Releases whose latest is `latest`.
    fn new(latest: &str) -> Releases {
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/releases", listener.local_addr().unwrap());
        let asked = Arc::new(Mutex::new(0));
        let (root, base, latest) = (dir.path().to_path_buf(), url.clone(), latest.to_string());
        let counted = asked.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                serve_release(stream, &root, &base, &latest, &counted);
            }
        });
        Releases { dir, url, asked }
    }

    fn asked(&self) -> usize {
        *self.asked.lock().unwrap()
    }

    /// Publishes the release `version`, its crystal the script `crystal`,
    /// with its checksum, or `sum` in its place.
    fn publish(&self, version: &str, crystal: &str, sum: Option<&str>) {
        use std::os::unix::fs::PermissionsExt;
        let name = format!("crystal-{version}-{}", release_target());
        let build = self.dir.path().join("build");
        std::fs::create_dir_all(build.join(&name)).unwrap();
        let binary = build.join(&name).join("crystal");
        std::fs::write(&binary, crystal).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let download = self.dir.path().join(format!("download/v{version}"));
        std::fs::create_dir_all(&download).unwrap();
        let archive = download.join(format!("{name}.tar.gz"));
        let tar = outside_crystal("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&build)
            .arg(&name)
            .status()
            .unwrap();
        assert!(tar.success());
        let sum = sum.map_or_else(
            || sha256_hex(&std::fs::read(&archive).unwrap()),
            String::from,
        );
        let line = format!("{sum}  {name}.tar.gz\n");
        std::fs::write(download.join(format!("{name}.tar.gz.sha256")), line).unwrap();
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Answers one request to the releases' web server.
fn serve_release(
    mut stream: std::net::TcpStream,
    root: &Path,
    base: &str,
    latest: &str,
    asked: &Mutex<usize>,
) {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    let _ = reader.read_line(&mut request);
    let mut header = String::new();
    while reader.read_line(&mut header).is_ok_and(|read| read > 2) {
        header.clear();
    }
    let mut words = request.split_whitespace();
    let (method, path) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    let file = path
        .strip_prefix("/releases/download/")
        .and_then(|file| std::fs::read(root.join("download").join(file)).ok());
    let (head, body) = if path == "/releases/latest" {
        *asked.lock().unwrap() += 1;
        let location = format!("{base}/tag/v{latest}");
        (format!("302 Found\r\nLocation: {location}"), Vec::new())
    } else if let Some(body) = file {
        ("200 OK".to_string(), body)
    } else {
        ("404 Not Found".to_string(), Vec::new())
    };
    let head = format!(
        "HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    if method != "HEAD" {
        let _ = stream.write_all(&body);
    }
}

/// A crystal for a release that only says what it was asked to do, in
/// `log`, and its version, 9.9.9.
fn logging_crystal(log: &Path) -> String {
    format!(
        "#!/bin/sh\necho \"$*\" >> '{}'\nif [ \"$1\" = --version ]; then echo 'crystal 9.9.9'; fi\n",
        log.display()
    )
}

/// A copy of the crystal under test in `dir`'s `bin`, for an update to
/// replace.
fn installed_copy(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let installed = bin.join("crystal");
    std::fs::copy(CRYSTAL, &installed).unwrap();
    installed
}

#[test]
fn update_puts_the_latest_release_in_place_and_restarts_every_daemon_on_it() {
    let servers = Servers::new();
    servers.ok(&["new", "-d", "-n", "keeper", "sleep", "300"]);
    servers.ok(&[
        "--server", "work", "new", "-d", "-n", "other", "sleep", "300",
    ]);
    let releases = Releases::new("9.9.9");
    let log = servers.dir().join("new-crystal.log");
    releases.publish("9.9.9", &logging_crystal(&log), None);
    let installed = installed_copy(servers.dir());
    // Claude Code is on this machine: its config directory is there.
    std::fs::create_dir_all(servers.crystal.claude_config_dir()).unwrap();

    let out = servers
        .command_of(&installed, &["update"])
        .env("CRYSTAL_RELEASES", &releases.url)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        said.contains(&format!("updated crystal {VERSION} to 9.9.9")),
        "{said}"
    );
    assert_eq!(
        std::fs::read_to_string(&installed).unwrap(),
        logging_crystal(&log)
    );
    // Nothing is left beside it.
    let beside = std::fs::read_dir(installed.parent().unwrap()).unwrap();
    assert_eq!(beside.count(), 1);

    // The new crystal was tried, then restarted each daemon and installed
    // its skill.
    let ran = std::fs::read_to_string(&log).unwrap();
    let ran: Vec<&str> = ran.lines().collect();
    assert_eq!(ran[0], "--version", "{ran:?}");
    for server in ["default", "work"] {
        let socket = servers.run_dir().join(format!("crystal/{server}.sock"));
        let restart = format!("--socket {} restart-server", socket.display());
        assert!(ran.contains(&restart.as_str()), "{ran:?}");
    }
    assert_eq!(ran.last(), Some(&"skill --install"), "{ran:?}");
}

#[test]
fn an_update_that_doesnt_match_its_checksum_changes_nothing() {
    let crystal = Crystal::new();
    let releases = Releases::new("9.9.9");
    let log = crystal.dir.path().join("new-crystal.log");
    releases.publish("9.9.9", &logging_crystal(&log), Some(&"0".repeat(64)));
    let installed = installed_copy(crystal.dir.path());

    let out = crystal
        .command_of(&installed, &["update"])
        .env("CRYSTAL_RELEASES", &releases.url)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("doesn't match its checksum"), "{err}");
    // The crystal there is the one that was, and the new one never ran.
    let version = outside_crystal(&installed)
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!("crystal {VERSION}\n")
    );
    assert!(!log.exists());
    let beside = std::fs::read_dir(installed.parent().unwrap()).unwrap();
    assert_eq!(beside.count(), 1);
}

#[test]
fn update_check_says_whether_a_newer_crystal_is_out() {
    let crystal = Crystal::new();
    let said = |releases: &Releases, args: &[&str]| {
        let out = crystal
            .command(args)
            .env("CRYSTAL_RELEASES", &releases.url)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let newer = Releases::new("9.9.9");
    assert_eq!(
        said(&newer, &["update", "--check"]),
        format!("crystal 9.9.9 is out, and this is {VERSION}: `crystal update` installs it\n")
    );
    let same = Releases::new(VERSION);
    let latest = format!("crystal {VERSION} is the latest\n");
    assert_eq!(said(&same, &["update", "--check"]), latest);
    // With nothing newer, an update has nothing to do.
    assert_eq!(said(&same, &["update"]), latest);
}

#[test]
fn the_tui_says_once_a_day_that_a_newer_crystal_is_out() {
    let crystal = Crystal::new();
    let releases = Releases::new("9.9.9");
    let env = [("CRYSTAL_RELEASES", releases.url.as_str())];
    let mut tui = crystal.attach_with_env(&[], &env);
    tui.shows("crystal 9.9.9 is out: `crystal update` installs it");
    assert_eq!(releases.asked(), 1);
    // Any key and it's gone.
    tui.type_keys("j");
    tui.hides("is out");
    tui.type_keys("q");
    assert!(tui.exit());

    // Opened again the same day, it doesn't ask again.
    let tui = crystal.attach_with_env(&[], &env);
    tui.shows("crystal");
    thread::sleep(Duration::from_millis(500));
    assert_eq!(releases.asked(), 1);
    assert!(!tui.text().contains("is out"));
}

#[test]
fn bash_completes_the_names_of_running_sessions() {
    let crystal = Crystal::new();
    let dir = crystal.dir.path();
    std::fs::write(
        dir.join("crystal.bash"),
        crystal.ok(&["completions", "bash"]),
    )
    .unwrap();
    let driver = r#"
source ./crystal.bash
crystal() { printf 'review\nreviewer\nbuild\n'; }
at() {
    COMP_WORDS=("$@")
    COMP_CWORD=$((${#COMP_WORDS[@]} - 1))
    COMPREPLY=()
    _crystal_with_sessions crystal "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"
    echo "${COMPREPLY[*]}"
}
at crystal attach rev
at crystal a b
at crystal -L work kill ''
at crystal pane split --beside r
at crystal done -n b
at crystal send review ''
at crystal new -n ''
"#;
    let out = outside_crystal("bash")
        .arg("-c")
        .arg(driver)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(
        lines[..5],
        [
            "review reviewer",
            "build",
            "review reviewer build",
            "review reviewer",
            "build"
        ]
    );
    // A message to send, and a new session's name, aren't sessions.
    assert!(!lines[5].contains("review"), "{said}");
    assert!(!lines[6].contains("review"), "{said}");
}

#[test]
fn zsh_and_fish_completions_offer_the_sessions() {
    let crystal = Crystal::new();
    let zsh = crystal.ok(&["completions", "zsh"]);
    assert!(zsh.contains("_crystal_sessions"));
    if Path::new("/bin/zsh").exists() {
        let file = crystal.dir.path().join("_crystal");
        std::fs::write(&file, &zsh).unwrap();
        let checked = outside_crystal("/bin/zsh")
            .arg("-n")
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stderr)
        );
    }
    let fish = crystal.ok(&["completions", "fish"]);
    assert!(fish.contains("-a \"(crystal complete-sessions)\""));
}

#[test]
fn complete_sessions_lists_running_sessions_and_never_starts_the_daemon() {
    let crystal = Crystal::new();
    assert_eq!(crystal.ok(&["complete-sessions"]), "");
    assert!(!crystal.socket.exists());
    crystal.ok(&["new", "-d", "-n", "review", "sleep", "30"]);
    crystal.ok(&["new", "-d", "-n", "build", "sleep", "30"]);
    let listed = crystal.ok(&["complete-sessions"]);
    let mut names: Vec<&str> = listed.lines().collect();
    names.sort_unstable();
    assert_eq!(names, ["build", "review"]);
}
