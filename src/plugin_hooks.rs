//! The daemon's side of plugins' `[[events]]`: when something happens in
//! crystal, the hooks of the plugins that asked for it run, each with the
//! event as JSON on its standard input and its name in `CRYSTAL_EVENT`.
//!
//! A plugin's hooks run one at a time, in the order things happened, on a
//! thread of the plugin's own, so a slow plugin holds up neither the daemon
//! nor the others. What a hook prints goes to the plugin's log. A hook that
//! runs past [`TIMEOUT`] is stopped, and a plugin whose hooks fail
//! [`FAILURES_TO_PAUSE`] times in a row is paused, with a notice, until the
//! user turns it on again.

use crate::config::Config;
use crate::notify::{self, Notice};
use crate::plugin_manifest;
use crate::plugins::{self, Context};
use crate::project;
use crate::protocol::{Activity, SessionInfo, TaskRecord};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long a hook may run before it's stopped.
const TIMEOUT: Duration = Duration::from_secs(30);

/// How many times in a row a plugin's hooks may fail before it's paused.
const FAILURES_TO_PAUSE: u32 = 5;

/// Something that happened, for the plugins that listen for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// One of [`plugin_manifest::EVENTS`].
    pub name: &'static str,
    /// What hooks read on their standard input.
    pub body: Value,
    pub context: Context,
}

impl Event {
    /// Something that happened to `session`: it started, came to wait on
    /// the user, finished a turn, or ended.
    pub fn about_session(name: &'static str, session: &SessionInfo) -> Event {
        let worktree = session.worktree.as_ref();
        let body = json!({
            "event": name,
            "session": {
                "name": session.name,
                "id": session.id,
                "command": session.command,
                "cwd": session.cwd,
                "project": worktree.map(|worktree| &worktree.project_path),
                "worktree": worktree.map(|worktree| &worktree.path),
                "branch": worktree.and_then(|worktree| worktree.branch.as_ref()),
                "activity": session.activity.map(|activity| activity.to_string()),
                "task": session.task.as_ref().map(|task| &task.goal),
            },
        });
        Event {
            name,
            body,
            context: Context::of_session(session),
        }
    }

    /// A task closed, in `cwd`.
    pub fn task_closed(cwd: &Path, task: &TaskRecord) -> Event {
        let context = Context {
            session: Some(task.session.clone()),
            ..Context::of_dir(cwd)
        };
        Event {
            name: "task.closed",
            body: json!({ "event": "task.closed", "task": task }),
            context,
        }
    }

    /// A worktree was made at `path`, or removed from there.
    pub fn about_worktree(created: bool, path: &Path, branch: Option<&str>) -> Event {
        let name = if created {
            "worktree.created"
        } else {
            "worktree.removed"
        };
        // A removed worktree's directory is gone, so git can't say which
        // project it was in.
        let project = created.then(|| project::of(path).path);
        let body = json!({
            "event": name,
            "worktree": { "path": path, "branch": branch, "project": project },
        });
        let context = Context {
            project,
            worktree: Some(path.to_path_buf()),
            ..Context::default()
        };
        Event {
            name,
            body,
            context,
        }
    }
}

/// What happened to a session between two looks at it, as the events the
/// plugins hear: `running` and `activity` before, and now.
pub fn session_changes(
    before: (bool, Option<Activity>),
    now: (bool, Option<Activity>),
) -> Vec<&'static str> {
    let mut changes = Vec::new();
    if now.1 != before.1 {
        match now.1 {
            Some(Activity::Waiting) => changes.push("session.waiting"),
            Some(Activity::Done) => changes.push("session.done"),
            _ => {}
        }
    }
    if before.0 && !now.0 {
        changes.push("session.ended");
    }
    changes
}

/// The hooks of every plugin, run off the daemon's own threads.
pub struct Hooks {
    socket: PathBuf,
    /// Each plugin's queue of hooks to run, by its name, made the first
    /// time it has one.
    queues: Mutex<HashMap<String, Sender<Job>>>,
}

/// One hook to run for one event.
struct Job {
    dir: PathBuf,
    command: Vec<String>,
    event: Arc<Event>,
}

impl Hooks {
    pub fn new(socket: &Path) -> Hooks {
        Hooks {
            socket: socket.to_path_buf(),
            queues: Mutex::default(),
        }
    }

    /// Hands `event` to the hooks of every plugin that's on and listens
    /// for it, to run in turn after the plugin's others.
    pub fn tell(&self, event: Event) {
        let config = Config::load().unwrap_or_default();
        let event = Arc::new(event);
        for (dir, manifest) in plugins::running(&config, &self.socket) {
            let hooks = manifest
                .events
                .iter()
                .filter(|hook| plugin_manifest::matches(&hook.on, event.name));
            for hook in hooks {
                let job = Job {
                    dir: dir.clone(),
                    command: hook.command.clone(),
                    event: event.clone(),
                };
                self.queue(&manifest.name).send(job).ok();
            }
        }
    }

    fn queue(&self, plugin: &str) -> Sender<Job> {
        let mut queues = self.queues.lock().unwrap();
        let queue = queues.entry(plugin.to_string()).or_insert_with(|| {
            let (queue, jobs) = mpsc::channel::<Job>();
            let socket = self.socket.clone();
            let plugin = plugin.to_string();
            thread::spawn(move || {
                let mut failures = 0;
                for job in jobs {
                    // Paused while these waited: none of them run.
                    if plugins::paused(&socket, &plugin).is_some() {
                        continue;
                    }
                    match run(&socket, &plugin, &job, TIMEOUT) {
                        Ok(()) => failures = 0,
                        Err(err) => {
                            failures += 1;
                            plugins::log(&socket, &plugin, &format!("{err:#}"));
                            if failures >= FAILURES_TO_PAUSE {
                                failures = 0;
                                pause(&socket, &plugin);
                            }
                        }
                    }
                }
            });
            queue
        });
        queue.clone()
    }
}

/// Runs one hook, with the event on its standard input and what it prints
/// in the plugin's log, and stops it if it runs past `timeout`.
fn run(socket: &Path, plugin: &str, job: &Job, timeout: Duration) -> Result<()> {
    let event = &job.event;
    plugins::log(
        socket,
        plugin,
        &format!("{}: {}", event.name, job.command.join(" ")),
    );
    let log = plugins::open_log(socket, plugin)?;
    let mut command = plugins::command(&job.dir, &job.command, socket, &event.context);
    command
        .env("CRYSTAL_EVENT", event.name)
        .stdin(Stdio::piped())
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut child = command.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // A hook that doesn't read its input is fine.
        let _ = writeln!(stdin, "{}", event.body);
    }
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("{} on {}: {status}", job.command.join(" "), event.name);
            }
            return Ok(());
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "{} on {}: still running after {}s, so it was stopped",
                job.command.join(" "),
                event.name,
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Pauses a plugin that keeps failing, and tells the user so, and how to
/// turn it back on.
fn pause(socket: &Path, plugin: &str) {
    let text = format!(
        "the {plugin} plugin failed {FAILURES_TO_PAUSE} times in a row and was paused: \
         see `crystal plugin log {plugin}`, then turn it back on with \
         `crystal plugin enable {plugin}`"
    );
    plugins::log(socket, plugin, &text);
    if let Err(err) = plugins::pause(socket, plugin, &text) {
        eprintln!("crystal daemon: couldn't pause the {plugin} plugin: {err:#}");
    }
    eprintln!("crystal daemon: {text}");
    notify::tell(Notice {
        session: plugin.to_string(),
        // It's waiting on the user to look at it.
        activity: Activity::Waiting,
        text,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{State, Worktree};
    use Activity::{Done, Idle, Waiting, Working};
    use std::fs;

    #[test]
    fn a_session_coming_to_need_the_user_is_one_event() {
        assert_eq!(
            session_changes((true, Some(Working)), (true, Some(Waiting))),
            ["session.waiting"]
        );
        assert_eq!(
            session_changes((true, Some(Working)), (true, Some(Done))),
            ["session.done"]
        );
        assert!(session_changes((true, Some(Waiting)), (true, Some(Waiting))).is_empty());
        assert!(session_changes((true, Some(Done)), (true, Some(Idle))).is_empty());
        assert_eq!(
            session_changes((true, Some(Working)), (false, Some(Done))),
            ["session.done", "session.ended"]
        );
        assert_eq!(
            session_changes((true, None), (false, None)),
            ["session.ended"]
        );
    }

    #[test]
    fn a_session_event_says_which_session_and_where() {
        let session = SessionInfo {
            name: "claude".into(),
            id: "s1".into(),
            command: vec!["claude".into()],
            cwd: "/code/app".into(),
            pid: None,
            state: State::Running,
            activity: Some(Waiting),
            worktree: Some(Worktree {
                project: "app".into(),
                project_path: "/code/app".into(),
                path: "/code/app".into(),
                main: true,
                branch: Some("main".into()),
            }),
            changed: 0,
            front: None,
            task: None,
            asking: None,
        };
        let event = Event::about_session("session.waiting", &session);
        assert_eq!(event.body["event"], "session.waiting");
        assert_eq!(event.body["session"]["name"], "claude");
        assert_eq!(event.body["session"]["branch"], "main");
        assert_eq!(event.body["session"]["activity"], "waiting");
        assert_eq!(event.context.session_id.as_deref(), Some("s1"));
    }

    fn job(dir: &Path, script: &str) -> Job {
        fs::write(dir.join("hook.sh"), script).unwrap();
        Job {
            dir: dir.to_path_buf(),
            command: vec!["sh".into(), "hook.sh".into()],
            event: Arc::new(Event {
                name: "session.done",
                body: json!({"event": "session.done"}),
                context: Context::default(),
            }),
        }
    }

    #[test]
    fn a_hook_reads_the_event_and_its_output_goes_to_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crystal.sock");
        let job = job(dir.path(), "echo \"got $CRYSTAL_EVENT\"; cat\n");
        run(&socket, "notes", &job, TIMEOUT).unwrap();
        let log = fs::read_to_string(plugins::log_path(&socket, "notes")).unwrap();
        assert!(log.contains("got session.done"), "{log}");
        assert!(log.contains(r#"{"event":"session.done"}"#), "{log}");
    }

    #[test]
    fn a_hook_that_fails_or_runs_too_long_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crystal.sock");
        let err = run(&socket, "notes", &job(dir.path(), "exit 3\n"), TIMEOUT).unwrap_err();
        assert!(err.to_string().contains("exit status: 3"), "{err:#}");
        let slow = job(dir.path(), "exec sleep 5\n");
        let err = run(&socket, "notes", &slow, Duration::from_millis(100)).unwrap_err();
        assert!(err.to_string().contains("stopped"), "{err:#}");
    }
}
