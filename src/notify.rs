//! Telling the user when a session needs them while they're looking at
//! something else: its agent is asking them something, or has finished a
//! turn nobody was watching.
//!
//! The daemon checks its sessions as it keeps up with them, and tells the
//! user once each time a session comes to need them: at once, or once it
//! has gone on needing them for `[notifications] after_secs`; and with
//! `unfocused_only`, only while no crystal TUI's terminal has the focus. A
//! session shown in a TUI whose terminal has lost the focus isn't being
//! watched. `crystal notify` tells the user something of its own, under
//! a title of its own if it likes, with either sound or none.
//!
//! A desktop notification does the telling: on macOS `terminal-notifier`
//! when it's installed, which a click on takes the user to the session, or
//! else macOS's own; on Linux `notify-send`, clicked the same way where it
//! takes actions, its text escaped where the notification server would
//! read markup in it. A command in the config runs instead, when it names
//! one, and finds the notice in its environment:
//!
//! - `CRYSTAL_NOTICE`: the line a notification would show, like
//!   "claude-2 is waiting on you · app fix/login"
//! - `CRYSTAL_NOTICE_TITLE`: its title: `crystal`, or the one `crystal
//!   notify` gave it
//! - `CRYSTAL_NOTICE_SESSION`: the session's name
//! - `CRYSTAL_NOTICE_ACTIVITY`: `waiting` or `done`
//! - `CRYSTAL_NOTICE_JUMP`: a shell command that takes the user to the
//!   session in the TUI and brings its terminal to the front, for a click
//!
//! A sound plays at the same moments, unless `[sound]` says not to: see
//! [`crate::sound`]. It goes with the notifications plugin, but not with
//! `notify`, so the one can be on without the other. `crystal notify` plays
//! the one for an agent asking, unless it says which, or none.

use crate::agents;
use crate::config::Config;
use crate::printable;
use crate::protocol::{Activity, Front, NotifySound, SessionInfo};
use crate::sound::{self, Sound};
use crate::tui::status_bar;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// How long telling the user may take before it's given up on.
const TELL_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a notification that can be clicked is listened to for a
/// click, once it's shown.
const CLICK_WITHIN: Duration = Duration::from_secs(60 * 60);

/// How long asking the notification server what it can do may take.
const CAPABILITIES_TIMEOUT: Duration = Duration::from_secs(3);

/// Something to tell the user about one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub session: String,
    /// What the session's agent is doing: waiting or done.
    pub activity: Activity,
    /// What a notification is titled: crystal's name, unless it's given
    /// another.
    pub title: Option<String>,
    /// The line a notification shows.
    pub text: String,
    /// The session a click on it takes the user to, if any.
    pub jump: Option<String>,
    /// The program of the session's agent, like `claude`, for the sounds
    /// switched off for some agents.
    pub agent: Option<String>,
    /// The sound that plays with it, if any.
    pub sound: Option<Sound>,
}

impl Notice {
    pub fn about(session: &SessionInfo, activity: Activity) -> Notice {
        let what = match activity {
            Activity::Waiting => "is waiting on you",
            _ => "is done",
        };
        let mut text = format!("{} {what}", session.name);
        // What an agent that reports for itself says it waits on them for.
        let message = session.reporter.as_ref().and_then(|r| r.message.as_ref());
        if let Some(message) = message.filter(|_| activity == Activity::Waiting) {
            text.push_str(&format!(": {message}"));
        }
        if let Some(worktree) = &session.worktree {
            text.push_str(&format!(" · {}", worktree.project));
            if let Some(branch) = &worktree.branch {
                text.push_str(&format!(" {branch}"));
            }
        }
        Notice {
            session: session.name.clone(),
            activity,
            title: None,
            // A notification shows it, and the user's own command may
            // print it to a terminal.
            text: printable::line(&text).into_owned(),
            jump: Some(session.name.clone()),
            agent: agent_of(session),
            sound: Some(Sound::of(activity)),
        }
    }

    /// `text`, said by crystal itself, as waiting on the user: about the
    /// session called `session` when it's about one, which a click on it
    /// takes them to.
    pub fn of_crystal(text: String, session: Option<String>) -> Notice {
        Notice {
            session: session.clone().unwrap_or_default(),
            activity: Activity::Waiting,
            title: None,
            text,
            jump: session,
            agent: None,
            sound: Some(Sound::Request),
        }
    }

    /// What `crystal notify` was asked to tell the user: `text` under
    /// `title`, about the session called `session` when it's about one,
    /// with `sound`. With no text, the title is what it says, under
    /// crystal's name; with neither, there's nothing to tell. Each is made
    /// one line fit for a notification.
    pub fn told(
        title: Option<&str>,
        text: &str,
        session: Option<String>,
        sound: NotifySound,
    ) -> Option<Notice> {
        let tidy = |text: &str| printable::line(text.trim()).trim().to_string();
        let title = title.map(tidy).filter(|title| !title.is_empty());
        let text = tidy(text);
        let (title, text) = match (title, text.is_empty()) {
            (None, true) => return None,
            (Some(title), true) => (None, title),
            (title, false) => (title, text),
        };
        let text = match &session {
            Some(session) => format!("{session}: {text}"),
            None => text,
        };
        let sound = match sound {
            NotifySound::None => None,
            NotifySound::Done => Some(Sound::Done),
            NotifySound::Request => Some(Sound::Request),
        };
        Some(Notice {
            title,
            sound,
            ..Notice::of_crystal(text, session)
        })
    }

    /// What it's titled.
    fn title(&self) -> &str {
        self.title.as_deref().unwrap_or("crystal")
    }
}

/// Whether the user is at crystal, by what its TUIs' terminals say of their
/// focus. `crystal layout --json` has it as `presence`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    /// No TUI says, or one whose terminal never says.
    #[default]
    Unknown,
    /// A TUI's terminal has the focus.
    Here,
    /// Every TUI's terminal has lost it.
    Away,
}

/// Where the user is, as the daemon's TUIs last said: see
/// [`crate::layout_relay`], which keeps it.
static PRESENCE: AtomicU8 = AtomicU8::new(0);

pub fn set_presence(presence: Presence) {
    let value = match presence {
        Presence::Unknown => 0,
        Presence::Here => 1,
        Presence::Away => 2,
    };
    PRESENCE.store(value, Ordering::Relaxed);
}

pub fn presence() -> Presence {
    match PRESENCE.load(Ordering::Relaxed) {
        1 => Presence::Here,
        2 => Presence::Away,
        _ => Presence::Unknown,
    }
}

/// Whether a session with a viewer when `viewed` is watched: shown
/// somewhere, and not only in TUIs whose terminals have all lost the focus.
pub fn watching(viewed: bool) -> bool {
    viewed && presence() != Presence::Away
}

/// What the user has been told about one session, and what's waiting to be
/// told.
#[derive(Debug, Clone, Default)]
pub struct Telling {
    /// What the user was last told about the session, or saw, while it
    /// still holds: it waits on them, or it's done.
    pub told: Option<Activity>,
    /// What it has come to need them for, and since when, while that
    /// waits [`NotifySettings::after_secs`](crate::config::NotifySettings)
    /// to be told.
    held: Option<(Activity, Instant)>,
}

impl Telling {
    /// Picks up from what a daemon that handed over had told the user.
    pub fn after(told: Option<Activity>) -> Telling {
        Telling { told, held: None }
    }

    /// Whether to tell the user, at `at`, that a session's agent is doing
    /// `now`, `after` how long it has to go on doing it first. Each time a
    /// session comes to need them is told once, and one someone is watching
    /// never: they've seen it, so it isn't news later either.
    pub fn update(
        &mut self,
        now: Option<Activity>,
        watched: bool,
        at: Instant,
        after: impl FnOnce() -> Duration,
    ) -> bool {
        let Some(activity) = now.filter(|_| needs_user(now)) else {
            *self = Telling::default();
            return false;
        };
        if self.told == Some(activity) || watched {
            self.told = Some(activity);
            self.held = None;
            return false;
        }
        let since = match self.held {
            Some((held, since)) if held == activity => since,
            _ => at,
        };
        if at.saturating_duration_since(since) < after() {
            self.held = Some((activity, since));
            return false;
        }
        self.told = Some(activity);
        self.held = None;
        true
    }
}

/// The program of the agent in `session`: the one in front, or that
/// reports for itself, or else the program it was started with.
fn agent_of(session: &SessionInfo) -> Option<String> {
    match &session.front {
        Some(Front::Agent { program, .. }) => Some(program.clone()),
        Some(Front::Task) => Some("claude".to_string()),
        _ => agents::program_name(&session.command).map(String::from),
    }
}

/// Whether an agent doing `activity` needs the user: it's asking them
/// something, or it has finished a turn they haven't seen.
pub fn needs_user(activity: Option<Activity>) -> bool {
    matches!(activity, Some(Activity::Waiting | Activity::Done))
}

/// Tells the user, on a thread of its own, so that a slow or broken
/// notifier can never hold up the daemon, a click on it going to the
/// daemon at `socket`. What goes wrong goes to the daemon's log.
pub fn tell(notice: Notice, socket: &Path) {
    let socket = socket.to_path_buf();
    thread::spawn(move || {
        if let Err(err) = tell_now(&notice, &socket) {
            eprintln!(
                "crystal daemon: couldn't tell the user that {}: {err:#}",
                notice.text
            );
        }
    });
}

/// Whether notifications are on: the `notifications` plugin, and the
/// `notify` setting, which came first and still works. Either one off
/// keeps crystal quiet.
pub fn enabled(config: &Config) -> bool {
    config.notify && crate::plugins::enabled(config, "notifications")
}

/// The settings in the file, read each time, so that a change to it counts
/// straight away.
pub fn settings() -> Config {
    Config::load().unwrap_or_else(|err| {
        eprintln!("crystal daemon: {err:#}; using the default settings");
        Config::default()
    })
}

/// Whether to tell the user now, by `config`: notifications are on, and the
/// user isn't at crystal when they asked to be told only when they aren't.
pub fn may_tell(config: &Config, presence: Presence) -> bool {
    enabled(config) && !(config.notifications.unfocused_only && presence == Presence::Here)
}

/// Tells the user of `notice`: a sound, unless `[sound]` says not to, and
/// a notification, by the notification settings.
fn tell_now(notice: &Notice, socket: &Path) -> Result<()> {
    let config = settings();
    if let Some(sound) = notice.sound
        && sound::wanted(&config, notice.agent.as_deref())
    {
        sound::play(sound, &config.sound);
    }
    if !may_tell(&config, presence()) {
        return Ok(());
    }
    let jump = notice
        .jump
        .as_deref()
        .map(|session| jump_argv(socket, session));
    let command = match &config.notify_command {
        Some(command) => user_command(command, notice, jump.as_deref()),
        None => match desktop_command(notice, jump.as_deref()) {
            Some(Desktop::Shown(command)) => command,
            Some(Desktop::Clickable(command)) => {
                return run_for_click(command, jump.as_deref());
            }
            // Nothing on this machine shows notifications.
            None => return Ok(()),
        },
    };
    run(command)
}

/// The command that takes the user to `session`: the TUI used last selects
/// it, handing it the keyboard, and brings its terminal to the front.
fn jump_argv(socket: &Path, session: &str) -> Vec<String> {
    let crystal = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("crystal"));
    [
        crystal.display().to_string(),
        "--socket".into(),
        socket.display().to_string(),
        "pane".into(),
        "focus".into(),
        "--raise".into(),
        "--".into(),
        session.into(),
    ]
    .into()
}

/// `argv` as a line the shell runs.
fn shell_line(argv: &[String]) -> String {
    let words: Vec<String> = argv.iter().map(|arg| crate::shell::quote(arg)).collect();
    words.join(" ")
}

/// The user's own command, run by the shell, with the notice in its
/// environment.
fn user_command(command: &str, notice: &Notice, jump: Option<&[String]>) -> Command {
    let mut user = Command::new("sh");
    user.arg("-c")
        .arg(command)
        .env("CRYSTAL_NOTICE", &notice.text)
        .env("CRYSTAL_NOTICE_TITLE", notice.title())
        .env("CRYSTAL_NOTICE_SESSION", &notice.session)
        .env("CRYSTAL_NOTICE_ACTIVITY", notice.activity.to_string());
    match jump {
        Some(jump) => user.env("CRYSTAL_NOTICE_JUMP", shell_line(jump)),
        None => user.env_remove("CRYSTAL_NOTICE_JUMP"),
    };
    user
}

/// A desktop notification's command.
enum Desktop {
    /// Shows it, a click doing whatever the desktop does.
    Shown(Command),
    /// Shows it and waits, printing `default` once it's clicked.
    Clickable(Command),
}

/// The desktop's own notification, where crystal knows how to show one,
/// one a click on runs `jump` where it can be.
fn desktop_command(notice: &Notice, jump: Option<&[String]>) -> Option<Desktop> {
    if cfg!(target_os = "macos") && on_path("terminal-notifier") {
        let mut notifier = Command::new("terminal-notifier");
        notifier
            .args(["-title", notice.title(), "-message"])
            .arg(&notice.text)
            // A newer notice about the session takes the place of the last.
            .args(["-group", &format!("crystal-{}", notice.session)]);
        if let Some(jump) = jump {
            notifier.arg("-execute").arg(shell_line(jump));
        }
        Some(Desktop::Shown(notifier))
    } else if cfg!(target_os = "macos") {
        // The text and the title go in as arguments, so nothing in them
        // needs escaping the way it would inside the script.
        let mut osascript = Command::new("osascript");
        osascript
            .args(["-e", "on run argv"])
            .args([
                "-e",
                "display notification (item 1 of argv) with title (item 2 of argv)",
            ])
            .args(["-e", "end run"])
            .arg(&notice.text)
            .arg(notice.title());
        Some(Desktop::Shown(osascript))
    } else if on_path("notify-send") {
        let mut notify_send = Command::new("notify-send");
        notify_send.args(["--app-name", "crystal"]);
        let clickable = jump.is_some() && notify_send_takes_actions();
        if clickable {
            notify_send.args(["--action", "default=Open", "--wait"]);
        }
        // A report's message or a branch holding `<b>` or `<a href>` would
        // be drawn as markup by a server that takes it.
        let body = if body_markup() {
            escape_markup(&notice.text)
        } else {
            notice.text.clone()
        };
        // The summary is never read as markup.
        notify_send.arg(notice.title()).arg(body);
        Some(if clickable {
            Desktop::Clickable(notify_send)
        } else {
            Desktop::Shown(notify_send)
        })
    } else {
        None
    }
}

/// Whether this machine's `notify-send` takes `--action`, as libnotify's
/// has since 0.7.10: asked once.
fn notify_send_takes_actions() -> bool {
    static TAKES: OnceLock<bool> = OnceLock::new();
    *TAKES.get_or_init(|| {
        let help = Command::new("notify-send")
            .arg("--help")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        help.is_ok_and(|help| String::from_utf8_lossy(&help.stdout).contains("--action"))
    })
}

/// Whether the desktop's notification server takes a notification's body
/// as markup: asked once.
fn body_markup() -> bool {
    static MARKUP: OnceLock<bool> = OnceLock::new();
    *MARKUP.get_or_init(|| says_body_markup(Path::new("gdbus"), Path::new("dbus-send")))
}

/// Whether the notification server says `body-markup` is among its
/// capabilities, as the freedesktop spec has one say that takes `<b>`,
/// `<a href>` and the like in a notification's body: asked with `gdbus`, or
/// `dbus-send` where that can't. A server neither can ask in
/// [`CAPABILITIES_TIMEOUT`] is taken not to, since escaping the body for
/// one that doesn't would show `&lt;` as it is.
fn says_body_markup(gdbus: &Path, dbus_send: &Path) -> bool {
    let mut with_gdbus = Command::new(gdbus);
    with_gdbus.args([
        "call",
        "--session",
        "--dest",
        "org.freedesktop.Notifications",
        "--object-path",
        "/org/freedesktop/Notifications",
        "--method",
        "org.freedesktop.Notifications.GetCapabilities",
    ]);
    let mut with_dbus_send = Command::new(dbus_send);
    with_dbus_send.args([
        "--session",
        "--print-reply",
        "--dest=org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications.GetCapabilities",
    ]);
    let said = [with_gdbus, with_dbus_send].iter_mut().find_map(|ask| {
        let output = status_bar::run_within(ask, CAPABILITIES_TIMEOUT)?;
        let said = String::from_utf8_lossy(&output.stdout);
        output.status.success().then(|| said.into_owned())
    });
    // `gdbus` prints `(['body', 'body-markup'],)`, and `dbus-send` a
    // `string "body-markup"` line for each.
    said.is_some_and(|said| said.contains("'body-markup'") || said.contains("\"body-markup\""))
}

/// `text` with what markup would read in it written as itself: `&`, `<`
/// and `>`.
fn escape_markup(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// Runs a notification that waits to be clicked, for [`CLICK_WITHIN`] at
/// most, and runs `jump` if it is.
fn run_for_click(mut command: Command, jump: Option<&[String]>) -> Result<()> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped");
    let deadline = Instant::now() + CLICK_WITHIN;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    };
    if !status.success() {
        bail!("the notifier {status}");
    }
    let mut said = String::new();
    stdout.read_to_string(&mut said)?;
    if let Some(jump) = jump.filter(|_| said.trim() == "default") {
        let (program, args) = jump.split_first().expect("a program");
        let mut jump = Command::new(program);
        jump.args(args).stderr(Stdio::null());
        run(jump)?;
    }
    Ok(())
}

pub fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
}

/// Runs `command`, which has nothing to read and its output nowhere but
/// the daemon's log, and stops it if it takes longer than [`TELL_TIMEOUT`].
fn run(mut command: Command) -> Result<()> {
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::null()).spawn()?;
    let deadline = Instant::now() + TELL_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            bail!("the notifier {status}");
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the notifier took longer than {}s", TELL_TIMEOUT.as_secs());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{State, Worktree};
    use Activity::*;
    use std::path::PathBuf;

    fn session(worktree: Option<Worktree>) -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            front: None,
            id: "1a2b".into(),
            name: "claude-2".into(),
            command: vec!["claude".into()],
            cwd: PathBuf::from("/code/app"),
            pid: Some(1),
            state: State::Running,
            activity: Some(Waiting),
            worktree,
            changed: 0,
            task: None,
            asking: None,
            reporter: None,
            subagents: 0,
            model: None,
            line: None,
            bell: false,
            unseen_copies: 0,
            context: None,
            output_waits: 0,
        }
    }

    /// Whether `telling` tells of `now` at `secs` seconds in, told after
    /// `after` seconds of it.
    fn tells(
        telling: &mut Telling,
        now: Option<Activity>,
        watched: bool,
        secs: u64,
        after: u64,
    ) -> bool {
        let start = *START.get_or_init(Instant::now);
        let at = start + Duration::from_secs(secs);
        telling.update(now, watched, at, || Duration::from_secs(after))
    }

    static START: OnceLock<Instant> = OnceLock::new();

    #[test]
    fn waiting_and_done_need_the_user_and_nothing_else_does() {
        assert!(tells(&mut Telling::default(), Some(Waiting), false, 0, 0));
        assert!(tells(
            &mut Telling::after(Some(Waiting)),
            Some(Done),
            false,
            0,
            0
        ));
        assert!(!tells(&mut Telling::default(), Some(Working), false, 0, 0));
        assert!(!tells(&mut Telling::default(), Some(Idle), false, 0, 0));
        assert!(!tells(&mut Telling::default(), None, false, 0, 0));
    }

    #[test]
    fn the_same_thing_is_told_once() {
        let mut telling = Telling::default();
        assert!(tells(&mut telling, Some(Waiting), false, 0, 0));
        assert!(!tells(&mut telling, Some(Waiting), false, 1, 0));
        // Working again, then waiting again, is news again.
        assert!(!tells(&mut telling, Some(Working), false, 2, 0));
        assert!(tells(&mut telling, Some(Waiting), false, 3, 0));
    }

    #[test]
    fn a_session_someone_is_watching_is_never_told_about() {
        let mut telling = Telling::default();
        assert!(!tells(&mut telling, Some(Waiting), true, 0, 0));
        // Seen, so it isn't news once they look away either.
        assert!(!tells(&mut telling, Some(Waiting), false, 1, 0));
    }

    #[test]
    fn a_wait_is_told_only_once_it_has_gone_on_long_enough() {
        let mut telling = Telling::default();
        assert!(!tells(&mut telling, Some(Waiting), false, 0, 30));
        assert!(!tells(&mut telling, Some(Waiting), false, 29, 30));
        assert!(tells(&mut telling, Some(Waiting), false, 30, 30));
        assert!(!tells(&mut telling, Some(Waiting), false, 31, 30));
    }

    #[test]
    fn a_wait_ended_or_seen_before_its_time_is_never_told() {
        let mut telling = Telling::default();
        assert!(!tells(&mut telling, Some(Waiting), false, 0, 30));
        assert!(!tells(&mut telling, Some(Working), false, 10, 30));
        // Its clock starts again.
        assert!(!tells(&mut telling, Some(Waiting), false, 20, 30));
        assert!(!tells(&mut telling, Some(Waiting), false, 49, 30));
        assert!(tells(&mut telling, Some(Waiting), false, 50, 30));

        let mut telling = Telling::default();
        assert!(!tells(&mut telling, Some(Done), false, 0, 30));
        assert!(!tells(&mut telling, Some(Done), true, 10, 30));
        assert!(!tells(&mut telling, Some(Done), false, 40, 30));
    }

    #[test]
    fn done_after_waiting_starts_the_clock_again() {
        let mut telling = Telling::default();
        assert!(!tells(&mut telling, Some(Waiting), false, 0, 30));
        assert!(!tells(&mut telling, Some(Done), false, 20, 30));
        assert!(!tells(&mut telling, Some(Done), false, 49, 30));
        assert!(tells(&mut telling, Some(Done), false, 50, 30));
    }

    #[test]
    fn being_at_crystal_keeps_it_quiet_only_when_asked() {
        let mut config = Config::default();
        assert!(may_tell(&config, Presence::Here));
        config.notifications.unfocused_only = true;
        assert!(!may_tell(&config, Presence::Here));
        assert!(may_tell(&config, Presence::Away));
        // A terminal that never says has to be taken as elsewhere.
        assert!(may_tell(&config, Presence::Unknown));
        config.notify = false;
        assert!(!may_tell(&config, Presence::Away));
    }

    #[test]
    fn a_click_runs_crystal_against_the_same_daemon() {
        let argv = jump_argv(Path::new("/tmp/my socket"), "claude-2");
        assert_eq!(
            argv[1..],
            [
                "--socket",
                "/tmp/my socket",
                "pane",
                "focus",
                "--raise",
                "--",
                "claude-2"
            ]
        );
        let line = shell_line(&argv);
        assert!(
            line.ends_with("'/tmp/my socket' pane focus --raise -- claude-2"),
            "{line}"
        );
    }

    #[test]
    fn a_notice_says_where_the_session_is() {
        let worktree = Worktree {
            project: "app".into(),
            project_path: PathBuf::from("/code/app"),
            path: PathBuf::from("/code/app.worktrees/fix-login"),
            main: false,
            branch: Some("fix/login".into()),
            in_progress: None,
        };
        let notice = Notice::about(&session(Some(worktree)), Waiting);
        assert_eq!(notice.text, "claude-2 is waiting on you · app fix/login");

        let notice = Notice::about(&session(None), Done);
        assert_eq!(notice.text, "claude-2 is done");
    }

    #[test]
    fn a_notice_of_the_user_s_own_has_its_title_and_sound() {
        let notice = Notice::told(Some("Deploy"), "it's out", None, NotifySound::Done).unwrap();
        assert_eq!(
            (notice.title(), notice.text.as_str()),
            ("Deploy", "it's out")
        );
        assert_eq!(notice.sound, Some(Sound::Done));
        assert_eq!(notice.jump, None);
        // About a session, a click goes there, and the text says which.
        let notice = Notice::told(None, "ready", Some("api".into()), NotifySound::None).unwrap();
        assert_eq!(
            (notice.title(), notice.text.as_str()),
            ("crystal", "api: ready")
        );
        assert_eq!(notice.jump.as_deref(), Some("api"));
        assert_eq!(notice.sound, None);
        // A title alone is what it says, under crystal's name.
        let notice = Notice::told(Some(" build failed "), "", None, NotifySound::Request).unwrap();
        assert_eq!(
            (notice.title(), notice.text.as_str()),
            ("crystal", "build failed")
        );
        // With neither, there's nothing to tell.
        assert_eq!(
            Notice::told(Some(" \n "), "\x07", None, NotifySound::None),
            None
        );
        // Each is one line, with nothing a terminal would take as an order.
        let notice =
            Notice::told(Some("a\x1b]0;x\x07\nb"), "c\nd", None, NotifySound::None).unwrap();
        assert_eq!((notice.title(), notice.text.as_str()), ("a]0;x b", "c d"));
    }

    #[test]
    fn a_notification_is_titled_as_its_notice_says() {
        let notice = Notice::told(Some("Deploy"), "it's out", None, NotifySound::None).unwrap();
        let command = match desktop_command(&notice, None) {
            Some(Desktop::Shown(command) | Desktop::Clickable(command)) => command,
            None => return,
        };
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"Deploy".to_string()), "{args:?}");
        assert!(!args.contains(&"crystal".to_string()), "{args:?}");
    }

    #[test]
    fn a_notice_says_which_agent_it_s_about() {
        assert_eq!(
            Notice::about(&session(None), Done).agent.as_deref(),
            Some("claude")
        );
        let mut codex = session(None);
        codex.command = vec!["sh".into()];
        codex.front = Some(Front::Agent {
            program: "codex".into(),
            name: "Codex".into(),
        });
        assert_eq!(
            Notice::about(&codex, Waiting).agent.as_deref(),
            Some("codex")
        );
    }

    #[test]
    fn a_notice_says_what_an_agent_that_reports_waits_for() {
        let mut waiting = session(None);
        waiting.reporter = Some(crate::protocol::Reporter {
            agent: "pi".into(),
            message: Some("approve the deploy".into()),
            resume: None,
            source: None,
        });
        let notice = Notice::about(&waiting, Waiting);
        assert_eq!(
            notice.text,
            "claude-2 is waiting on you: approve the deploy"
        );
        assert_eq!(Notice::about(&waiting, Done).text, "claude-2 is done");
    }

    /// What `gdbus` is asked for the notification server's capabilities.
    const GDBUS_ASKED: &str = "call --session --dest org.freedesktop.Notifications \
                               --object-path /org/freedesktop/Notifications \
                               --method org.freedesktop.Notifications.GetCapabilities";

    /// What `dbus-send` is asked for them.
    const DBUS_SEND_ASKED: &str = "--session --print-reply --dest=org.freedesktop.Notifications \
                                   /org/freedesktop/Notifications \
                                   org.freedesktop.Notifications.GetCapabilities";

    /// A stand-in for `gdbus` or `dbus-send`, `name` in `dir`, that prints
    /// `said` when it's asked `asked`, and fails when it's asked anything
    /// else, as a real one does with no notification server to ask.
    fn fake(dir: &Path, name: &str, asked: &str, said: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join(name);
        let script =
            format!("#!/bin/sh\n[ \"$*\" = '{asked}' ] || exit 1\ncat <<'EOF'\n{said}\nEOF\n");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program
    }

    #[test]
    fn markup_in_a_notice_is_written_as_itself() {
        assert_eq!(
            escape_markup("ok <b>now</b> & <a href=\"https://x\">here</a>"),
            "ok &lt;b&gt;now&lt;/b&gt; &amp; &lt;a href=\"https://x\"&gt;here&lt;/a&gt;"
        );
        assert_eq!(
            escape_markup("claude-2 is done · app fix/login"),
            "claude-2 is done · app fix/login"
        );
    }

    #[test]
    fn the_notification_server_says_whether_it_takes_markup() {
        let dir = tempfile::tempdir().unwrap();
        let nowhere = dir.path().join("nowhere");
        let takes = fake(
            dir.path(),
            "gdbus",
            GDBUS_ASKED,
            "(['actions', 'body', 'body-hyperlinks', 'body-markup', 'persistence'],)",
        );
        assert!(says_body_markup(&takes, &nowhere));
        let plain = fake(
            dir.path(),
            "gdbus-plain",
            GDBUS_ASKED,
            "(['actions', 'body', 'body-hyperlinks', 'persistence'],)",
        );
        assert!(!says_body_markup(&plain, &nowhere));
    }

    #[test]
    fn dbus_send_asks_where_gdbus_can_t() {
        let dir = tempfile::tempdir().unwrap();
        let nowhere = dir.path().join("nowhere");
        let dbus_send = fake(
            dir.path(),
            "dbus-send",
            DBUS_SEND_ASKED,
            "method return time=1759500000.1 sender=:1.25 -> destination=:1.150 serial=7 \
             reply_serial=2\n   array [\n      string \"body\"\n      string \"body-markup\"\n   ]",
        );
        assert!(says_body_markup(&nowhere, &dbus_send));
        // One with no server to ask fails, and the next is asked.
        let failing = fake(dir.path(), "gdbus", "never this", "");
        assert!(says_body_markup(&failing, &dbus_send));
        // Neither there: no markup, so nothing is escaped to show as it is.
        assert!(!says_body_markup(&nowhere, &nowhere));
    }

    #[test]
    fn a_notice_holds_nothing_a_terminal_would_take_as_an_order() {
        let mut waiting = session(Some(Worktree {
            project: "app\u{202e}".into(),
            project_path: PathBuf::from("/code/app"),
            path: PathBuf::from("/code/app"),
            main: true,
            branch: Some("main\u{9b}2J".into()),
            in_progress: None,
        }));
        waiting.reporter = Some(crate::protocol::Reporter {
            agent: "pi".into(),
            message: Some("ok\x1b]0;pwned\x07\nnext".into()),
            resume: None,
            source: None,
        });
        assert_eq!(
            Notice::about(&waiting, Waiting).text,
            "claude-2 is waiting on you: ok]0;pwned next · app main2J"
        );
    }
}
