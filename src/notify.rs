//! Telling the user when a session needs them while they're looking at
//! something else: its agent is asking them something, or has finished a
//! turn nobody was watching.
//!
//! The daemon checks its sessions as it keeps up with them, and tells the
//! user once each time a session comes to need them. A desktop notification
//! does the telling, macOS's own or `notify-send` on Linux when it's
//! installed, unless the config names a command to run instead. That
//! command finds the notice in its environment:
//!
//! - `CRYSTAL_NOTICE`: the line a notification would show, like
//!   "claude-2 is waiting on you · app fix/login"
//! - `CRYSTAL_NOTICE_SESSION`: the session's name
//! - `CRYSTAL_NOTICE_ACTIVITY`: `waiting` or `done`

use crate::config::Config;
use crate::protocol::{Activity, SessionInfo};
use anyhow::{Result, bail};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How long telling the user may take before it's given up on.
const TELL_TIMEOUT: Duration = Duration::from_secs(10);

/// Something to tell the user about one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub session: String,
    /// What the session's agent is doing: waiting or done.
    pub activity: Activity,
    /// The line a notification shows.
    pub text: String,
}

impl Notice {
    pub fn about(session: &SessionInfo, activity: Activity) -> Notice {
        let what = match activity {
            Activity::Waiting => "is waiting on you",
            _ => "is done",
        };
        let mut text = format!("{} {what}", session.name);
        if let Some(worktree) = &session.worktree {
            text.push_str(&format!(" · {}", worktree.project));
            if let Some(branch) = &worktree.branch {
                text.push_str(&format!(" {branch}"));
            }
        }
        Notice {
            session: session.name.clone(),
            activity,
            text,
        }
    }
}

/// Whether an agent doing `activity` needs the user: it's asking them
/// something, or it has finished a turn they haven't seen.
pub fn needs_user(activity: Option<Activity>) -> bool {
    matches!(activity, Some(Activity::Waiting | Activity::Done))
}

/// Whether to tell the user that a session's agent is now doing `now`,
/// given what they were last told about it. Each time a session comes to
/// need them is told once, and a session someone is watching never.
pub fn worth_telling(now: Option<Activity>, told: Option<Activity>, watched: bool) -> bool {
    needs_user(now) && now != told && !watched
}

/// Tells the user, on a thread of its own, so that a slow or broken
/// notifier can never hold up the daemon. What goes wrong goes to the
/// daemon's log.
pub fn tell(notice: Notice) {
    thread::spawn(move || {
        if let Err(err) = tell_now(&notice) {
            eprintln!(
                "crystal daemon: couldn't tell the user that {}: {err:#}",
                notice.text
            );
        }
    });
}

fn tell_now(notice: &Notice) -> Result<()> {
    // Read each time, so that a change to the file counts straight away.
    let config = Config::load().unwrap_or_else(|err| {
        eprintln!("crystal daemon: {err:#}; using the default settings");
        Config::default()
    });
    if !config.notify {
        return Ok(());
    }
    let command = match &config.notify_command {
        Some(command) => user_command(command, notice),
        None => match desktop_command(notice) {
            Some(command) => command,
            // Nothing on this machine shows notifications.
            None => return Ok(()),
        },
    };
    run(command)
}

/// The user's own command, run by the shell, with the notice in its
/// environment.
fn user_command(command: &str, notice: &Notice) -> Command {
    let mut user = Command::new("sh");
    user.arg("-c")
        .arg(command)
        .env("CRYSTAL_NOTICE", &notice.text)
        .env("CRYSTAL_NOTICE_SESSION", &notice.session)
        .env("CRYSTAL_NOTICE_ACTIVITY", notice.activity.to_string());
    user
}

/// The desktop's own notification, where crystal knows how to show one.
fn desktop_command(notice: &Notice) -> Option<Command> {
    if cfg!(target_os = "macos") {
        // The text goes in as an argument, so nothing in it needs escaping
        // the way it would inside the script.
        let mut osascript = Command::new("osascript");
        osascript
            .args(["-e", "on run argv"])
            .args([
                "-e",
                "display notification (item 1 of argv) with title \"crystal\"",
            ])
            .args(["-e", "end run"])
            .arg(&notice.text);
        Some(osascript)
    } else if on_path("notify-send") {
        let mut notify_send = Command::new("notify-send");
        notify_send.arg("crystal").arg(&notice.text);
        Some(notify_send)
    } else {
        None
    }
}

fn on_path(program: &str) -> bool {
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
            id: "1a2b".into(),
            name: "claude-2".into(),
            command: vec!["claude".into()],
            cwd: PathBuf::from("/code/app"),
            pid: Some(1),
            state: State::Running,
            activity: Some(Waiting),
            worktree,
        }
    }

    #[test]
    fn waiting_and_done_need_the_user_and_nothing_else_does() {
        assert!(worth_telling(Some(Waiting), None, false));
        assert!(worth_telling(Some(Done), Some(Waiting), false));
        assert!(!worth_telling(Some(Working), None, false));
        assert!(!worth_telling(Some(Idle), None, false));
        assert!(!worth_telling(None, None, false));
    }

    #[test]
    fn the_same_thing_is_told_once() {
        assert!(!worth_telling(Some(Waiting), Some(Waiting), false));
    }

    #[test]
    fn a_session_someone_is_watching_is_never_told_about() {
        assert!(!worth_telling(Some(Waiting), None, true));
    }

    #[test]
    fn a_notice_says_where_the_session_is() {
        let worktree = Worktree {
            project: "app".into(),
            project_path: PathBuf::from("/code/app"),
            path: PathBuf::from("/code/app.worktrees/fix-login"),
            main: false,
            branch: Some("fix/login".into()),
        };
        let notice = Notice::about(&session(Some(worktree)), Waiting);
        assert_eq!(notice.text, "claude-2 is waiting on you · app fix/login");

        let notice = Notice::about(&session(None), Done);
        assert_eq!(notice.text, "claude-2 is done");
    }
}
