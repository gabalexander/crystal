//! Worktree hooks: two programs of the user's the daemon runs once crystal
//! has made a worktree or removed one, so that a project can set up, and
//! tidy away, what a worktree has outside its directory, like a port, a
//! database or a route. They're the repository's settings, not crystal's,
//! so they're in git config, read afresh each time:
//!
//! ```sh
//! git config crystal.worktreeCreateHook /path/to/worktree-setup
//! git config crystal.worktreeDeleteHook /path/to/worktree-cleanup
//! ```
//!
//! `--global` sets one for every project, and a repository's own overrides
//! it. A hook is never read from a file in the worktree: that would run
//! whatever a clone brought with it. Adapted from docket's.
//!
//! They hear of worktrees from the daemon's [`Bus`]: `worktree.created`,
//! which whoever made one says once it's made, and `worktree.removed`,
//! which the daemon says once git has removed one. One hook runs at a time,
//! in the order things happened, on a thread of its own. It's run as it's
//! named, not by a shell, with the main worktree and the worktree as its
//! arguments, in the main worktree, since a worktree removed has gone, and
//! with `CRYSTAL_HOOK` (`worktree-create` or `worktree-delete`),
//! `CRYSTAL_WORKTREE` and `CRYSTAL_WORKTREE_BRANCH` in its environment.
//! What it prints goes to a log in the server's state directory. One that
//! fails, can't start, or runs past [`TIMEOUT`], when it's stopped with
//! everything it started, only says so, in a `worktree.hook_failed` event:
//! the worktree is made, or gone, already. A handover gives the hook
//! running a few seconds to finish, and drops those still to run.

use crate::event_log::Bus;
use crate::events::{Event, Filter, Kind, WorktreeAbout};
use crate::git;
use crate::handover::{self, HELPERS};
use crate::session;
use crate::state;
use anyhow::{Context, Result, bail};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// The git config key that names the hook run once a worktree is made.
pub const CREATE_KEY: &str = "crystal.worktreeCreateHook";

/// The git config key that names the hook run once a worktree is removed.
pub const DELETE_KEY: &str = "crystal.worktreeDeleteHook";

/// How long a hook may run before it's stopped, with everything it
/// started.
const TIMEOUT: Duration = Duration::from_secs(30);

/// How big the log may grow before it starts over.
const LOG_MAX: u64 = 1 << 20;

/// Runs the worktree hooks for every worktree made or removed that `bus`
/// hears of, from a thread of its own, for as long as the daemon runs.
pub fn follow(bus: &Arc<Bus>, socket: &Path) {
    let filter = Filter {
        kinds: vec![
            Kind::WorktreeCreated.name().to_string(),
            Kind::WorktreeRemoved.name().to_string(),
        ],
        ..Filter::default()
    };
    // Subscribed before the daemon answers anyone, as the plugins are.
    let first = bus.subscribe(filter.clone());
    let bus = Arc::downgrade(bus);
    let socket = socket.to_path_buf();
    thread::spawn(move || {
        let mut subscription = Some(first);
        while let Some(subscribed) = subscription
            .take()
            .or_else(|| bus.upgrade().map(|bus| bus.subscribe(filter.clone())))
        {
            for event in subscribed.feed {
                let Some(hook) = Hook::on(&event, &socket) else {
                    continue;
                };
                // A handover waits only for the hook running.
                if handover::underway() {
                    log(&socket, &format!("{}: dropped in a handover", hook.what()));
                    continue;
                }
                if let Err(err) = hook.run(&socket, TIMEOUT) {
                    let why = format!("{err:#}");
                    log(&socket, &why);
                    eprintln!("crystal daemon: {why}");
                    if let Some(bus) = bus.upgrade() {
                        bus.emit(Event::worktree_hook_failed(&hook.worktree, &why));
                    }
                }
            }
            eprintln!("crystal daemon: the worktree hooks fell behind, and missed worktrees");
        }
    });
}

/// Which of the two hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Create,
    Delete,
}

impl Which {
    /// What `CRYSTAL_HOOK` says.
    fn name(self) -> &'static str {
        match self {
            Which::Create => "worktree-create",
            Which::Delete => "worktree-delete",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Which::Create => CREATE_KEY,
            Which::Delete => DELETE_KEY,
        }
    }
}

/// A hook to run, on a worktree.
#[derive(Debug)]
struct Hook {
    which: Which,
    /// The program git config names.
    program: PathBuf,
    /// The repository's main worktree.
    project: PathBuf,
    worktree: WorktreeAbout,
}

impl Hook {
    /// The hook to run for `event`, if the repository it's about has one.
    /// A removed worktree whose directory is still there, which git had
    /// stopped knowing of, isn't tidied away under its files.
    fn on(event: &Event, socket: &Path) -> Option<Hook> {
        let which = match event.kind {
            Kind::WorktreeCreated => Which::Create,
            Kind::WorktreeRemoved => Which::Delete,
            _ => return None,
        };
        let worktree = event.worktree.clone()?;
        let project = worktree.project.clone()?;
        let program = git::config_path(&project, which.key())?;
        let hook = Hook {
            which,
            program,
            project,
            worktree,
        };
        if which == Which::Delete && hook.worktree.path.exists() {
            log(
                socket,
                &format!("{}: skipped, as it's still there", hook.what()),
            );
            return None;
        }
        Some(hook)
    }

    /// What it is, for the log: `the worktree create hook on ~/app.worktrees/fix`.
    fn what(&self) -> String {
        let which = match self.which {
            Which::Create => "create",
            Which::Delete => "delete",
        };
        let path = crate::shell::home_relative(&self.worktree.path);
        format!("the worktree {which} hook on {path}")
    }

    /// Runs it, what it prints going to the log, and stops it, and every
    /// process it started, if it runs past `timeout`. An error says what
    /// went wrong and the last line it printed.
    fn run(&self, socket: &Path, timeout: Duration) -> Result<()> {
        log(
            socket,
            &format!("{}: {}", self.what(), self.program.display()),
        );
        let mut log_file = open_log(socket)?;
        let from = log_file.seek(SeekFrom::End(0))?;
        let mut command = Command::new(&self.program);
        command
            .arg(&self.project)
            .arg(&self.worktree.path)
            .current_dir(&self.project)
            .env("CRYSTAL_HOOK", self.which.name())
            .env("CRYSTAL_WORKTREE", &self.worktree.path)
            .env(
                "CRYSTAL_WORKTREE_BRANCH",
                self.worktree.branch.as_deref().unwrap_or_default(),
            )
            .stdin(Stdio::null())
            // A file, not a pipe: something it starts and leaves running
            // can hold it open without holding up the wait.
            .stdout(log_file.try_clone()?)
            .stderr(log_file.try_clone()?)
            // A process group of its own, which is stopped whole.
            .process_group(0);
        let mut child = command.spawn().with_context(|| {
            format!("{} couldn't start {}", self.what(), self.program.display())
        })?;
        let _helper = HELPERS.started(child.id());
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() >= timeout {
                session::signal_group(child.id(), libc::SIGKILL);
                let _ = child.wait();
                bail!(
                    "{} was still running after {}s, so it was stopped{}",
                    self.what(),
                    timeout.as_secs(),
                    last_line(&mut log_file, from)
                );
            }
            thread::sleep(Duration::from_millis(20));
        };
        if !status.success() {
            bail!(
                "{} failed: {status}{}",
                self.what(),
                last_line(&mut log_file, from)
            );
        }
        Ok(())
    }
}

/// The last line a hook printed to the log, from `from` on, as `: <line>`,
/// or nothing when it printed nothing.
fn last_line(log: &mut File, from: u64) -> String {
    let mut printed = String::new();
    if log.seek(SeekFrom::Start(from)).is_err() || log.read_to_string(&mut printed).is_err() {
        return String::new();
    }
    let last = printed
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty());
    match last {
        Some(line) => format!(": {}", crate::printable::line(line)),
        None => String::new(),
    }
}

/// The log, open to read and add to, made if there's none yet. One that
/// has grown past [`LOG_MAX`] starts over.
fn open_log(socket: &Path) -> Result<File> {
    let path = state::worktree_hooks_log(socket);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_MAX) {
        fs::remove_file(&path)?;
    }
    OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("couldn't open {}", path.display()))
}

/// Adds a line to the log.
fn log(socket: &Path, line: &str) {
    if let Ok(mut file) = open_log(socket) {
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A hook in `dir` that runs `script`.
    fn hook(dir: &Path, which: Which, script: &str) -> Hook {
        let program = dir.join("hook.sh");
        fs::write(&program, format!("#!/bin/sh\n{script}")).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        Hook {
            which,
            program,
            project: dir.to_path_buf(),
            worktree: WorktreeAbout {
                path: dir.join("app.worktrees/fix"),
                branch: Some("fix".into()),
                project: Some(dir.to_path_buf()),
                why: None,
            },
        }
    }

    #[test]
    fn a_hook_gets_the_project_the_worktree_and_which_hook_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crystal.sock");
        let hook = hook(
            dir.path(),
            Which::Create,
            "echo \"$CRYSTAL_HOOK $CRYSTAL_WORKTREE_BRANCH in $(pwd): $1 $2\"\n",
        );
        hook.run(&socket, TIMEOUT).unwrap();
        let log = fs::read_to_string(state::worktree_hooks_log(&socket)).unwrap();
        let project = fs::canonicalize(dir.path()).unwrap();
        let said = format!(
            "worktree-create fix in {}: {} {}",
            project.display(),
            dir.path().display(),
            dir.path().join("app.worktrees/fix").display()
        );
        assert!(log.contains(&said), "{log}");
    }

    #[test]
    fn a_hook_that_fails_says_why_with_its_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crystal.sock");
        let failing = hook(
            dir.path(),
            Which::Delete,
            "echo 'no slot for fix' >&2\nexit 3\n",
        );
        let err = failing.run(&socket, TIMEOUT).unwrap_err().to_string();
        assert!(err.contains("worktree delete hook"), "{err}");
        assert!(err.contains("exit status: 3: no slot for fix"), "{err}");

        let slow = hook(dir.path(), Which::Create, "sleep 5 &\nexec sleep 5\n");
        let started = Instant::now();
        let err = slow.run(&socket, Duration::from_millis(200)).unwrap_err();
        assert!(err.to_string().contains("stopped"), "{err:#}");
        assert!(started.elapsed() < Duration::from_secs(4));

        let missing = Hook {
            program: dir.path().join("nowhere"),
            ..hook(dir.path(), Which::Create, "")
        };
        let err = missing.run(&socket, TIMEOUT).unwrap_err().to_string();
        assert!(err.contains("couldn't start"), "{err}");
    }

    #[test]
    fn only_a_repository_that_names_a_hook_runs_one() {
        let dir = crate::git::tests::repo();
        let socket = dir.path().join("crystal.sock");
        // The repository's own worktree stands in for one made.
        let made = Event::worktree(true, dir.path(), Some("main"));
        assert!(Hook::on(&made, &socket).is_none());
        let set = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["config", CREATE_KEY, "~/bin/setup"])
            .status()
            .unwrap();
        assert!(set.success());
        let hook = Hook::on(&made, &socket).unwrap();
        assert_eq!(hook.which, Which::Create);
        assert!(hook.program.ends_with("bin/setup"));
        assert!(!hook.program.starts_with("~"), "{}", hook.program.display());
        // Only the create hook is set.
        let project = made.project.clone().unwrap();
        let removed = Event::worktree_removed(&dir.path().join("fix"), Some("fix"), &project);
        assert!(Hook::on(&removed, &socket).is_none());
    }
}
