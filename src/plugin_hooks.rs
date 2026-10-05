//! The daemon's side of plugins' `[[events]]` and `[[startup]]`: when
//! something happens in crystal, the hooks of the plugins that asked for it
//! run, each with the event as JSON on its standard input and in
//! `CRYSTAL_EVENT_JSON`, its name in `CRYSTAL_EVENT`, and what happened in
//! a line in `CRYSTAL_EVENT_TEXT`. They hear it from the daemon's [`Bus`],
//! like any other subscriber; a project's plugin hears only what happens
//! in its project. As the daemon starts, once it has brought back its
//! sessions, each plugin's startup commands run, with `CRYSTAL_EVENT` set
//! to `startup`.
//!
//! A plugin's commands run one at a time, in the order things happened, on
//! a thread of the plugin's own, so a slow plugin holds up neither the
//! daemon nor the others. What a command prints goes to the plugin's log.
//! One that runs past its timeout, [`plugin_manifest::TIMEOUT`] unless the
//! plugin says, is stopped, and a plugin whose commands fail
//! [`FAILURES_TO_PAUSE`] times in a row is paused, with a notice and a
//! `plugin.paused` event, until the user turns it on again. A handover to a
//! new crystal gives the commands running a few seconds to finish, and drops
//! those still waiting to run.

use crate::config::Config;
use crate::event_log::Bus;
use crate::events::{Event, Filter};
use crate::handover::{self, HELPERS};
use crate::notify::{self, Notice};
use crate::output::errln;
use crate::plugin_manifest::{self, Manifest};
use crate::plugins::{self, Context, Id};
use crate::shell;
use anyhow::{Context as _, Result, bail};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

/// How many times in a row a plugin's hooks may fail before it's paused.
const FAILURES_TO_PAUSE: u32 = 5;

/// Hands every event on `bus` to the hooks of the plugins that listen for
/// it, from a thread of its own, for as long as the daemon runs. The hooks
/// it returns run the plugins' startup commands too: see
/// [`Hooks::start_up`].
pub fn follow(bus: &Arc<Bus>, socket: &Path) -> Arc<Hooks> {
    let hooks = Arc::new(Hooks::new(socket, Arc::downgrade(bus)));
    let following = hooks.clone();
    // Subscribed before the daemon answers anyone, so the hooks hear what
    // its first requests do.
    let first = bus.subscribe(Filter::default());
    let bus = Arc::downgrade(bus);
    thread::spawn(move || {
        let mut subscription = Some(first);
        while let Some(subscribed) = subscription
            .take()
            .or_else(|| bus.upgrade().map(|bus| bus.subscribe(Filter::default())))
        {
            for event in subscribed.feed {
                following.tell(&event);
            }
            // Dropped for falling behind: what was missed is lost, and the
            // hooks hear what happens from now on.
            errln!("crystal daemon: the plugins fell behind, and missed events");
        }
    });
    hooks
}

/// The commands of every plugin, run off the daemon's own threads.
pub struct Hooks {
    socket: PathBuf,
    /// Where a plugin paused for failing says so.
    bus: Weak<Bus>,
    /// Each plugin's queue of hooks to run, made the first time it has
    /// one.
    queues: Mutex<HashMap<Id, Sender<Job>>>,
}

/// One of a plugin's commands to run: a hook for one event, or a startup
/// command.
struct Job {
    dir: PathBuf,
    command: Vec<String>,
    /// The event it's run on, or `None` as the daemon starts.
    event: Option<Arc<Event>>,
    /// How long it may run before it's stopped.
    timeout: Duration,
}

impl Job {
    /// What it's run on, for `CRYSTAL_EVENT` and the log: the event's name,
    /// or `startup`.
    fn on(&self) -> &'static str {
        self.event
            .as_ref()
            .map_or("startup", |event| event.kind.name())
    }
}

impl Hooks {
    fn new(socket: &Path, bus: Weak<Bus>) -> Hooks {
        Hooks {
            socket: socket.to_path_buf(),
            bus,
            queues: Mutex::default(),
        }
    }

    /// Hands `event` to the hooks of every plugin that's on and listens
    /// for it, to run in turn after the plugin's others: a project's
    /// plugin, only when it's about that project.
    fn tell(&self, event: &Arc<Event>) {
        let config = Config::load().unwrap_or_default();
        for plugin in plugins::running(&config, &self.socket) {
            if !hears(&plugin.id, event) {
                continue;
            }
            for hook in hooks_on(&plugin.manifest, event) {
                let job = Job {
                    dir: plugin.dir.clone(),
                    command: hook.command.clone(),
                    event: Some(event.clone()),
                    timeout: plugin.manifest.timeout(hook.timeout_secs),
                };
                self.queue(&plugin.id).send(job).ok();
            }
        }
    }

    /// Runs the startup commands of every plugin that's on and can run
    /// here, in turn with its plugin's hooks: once as the daemon starts,
    /// after it has brought back its sessions, and again whenever a daemon
    /// takes over from another.
    pub fn start_up(&self) {
        let config = Config::load().unwrap_or_default();
        for plugin in plugins::running(&config, &self.socket) {
            let manifest = &plugin.manifest;
            for once in manifest.startup.iter().filter(|once| once.runs_here()) {
                let job = Job {
                    dir: plugin.dir.clone(),
                    command: once.command.clone(),
                    event: None,
                    timeout: manifest.timeout(once.timeout_secs),
                };
                self.queue(&plugin.id).send(job).ok();
            }
        }
    }

    fn queue(&self, plugin: &Id) -> Sender<Job> {
        let mut queues = self.queues.lock().unwrap();
        let queue = queues.entry(plugin.clone()).or_insert_with(|| {
            let (queue, jobs) = mpsc::channel::<Job>();
            let socket = self.socket.clone();
            let bus = self.bus.clone();
            let plugin = plugin.clone();
            thread::spawn(move || {
                let mut failures = 0;
                for job in jobs {
                    // Paused while these waited: none of them run.
                    if plugins::paused(&socket, &plugin).is_some() {
                        continue;
                    }
                    // A handover waits only for the commands running.
                    if handover::underway() {
                        let name = job.on();
                        plugins::log(&socket, &plugin, &format!("{name}: dropped in a handover"));
                        continue;
                    }
                    match run(&socket, &plugin, &job) {
                        Ok(()) => failures = 0,
                        Err(err) => {
                            failures += 1;
                            plugins::log(&socket, &plugin, &format!("{err:#}"));
                            if failures >= FAILURES_TO_PAUSE {
                                failures = 0;
                                pause(&socket, &bus, &plugin);
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

/// Whether the plugin `plugin` hears `event`: the user's own hear every
/// one, and a project's those about its project.
fn hears(plugin: &Id, event: &Event) -> bool {
    match (&plugin.project, &event.project) {
        (None, _) => true,
        (Some(project), Some(about)) => plugins::same_dir(project, about),
        (Some(_), None) => false,
    }
}

/// The hooks of `manifest` that listen for `event`.
pub fn hooks_on<'a>(
    manifest: &'a Manifest,
    event: &Event,
) -> impl Iterator<Item = &'a plugin_manifest::EventHook> {
    let name = event.kind.name();
    manifest
        .events
        .iter()
        .filter(move |hook| plugin_manifest::matches(&hook.on, name))
}

/// The plugin `plugin`'s `job`, from its directory, with what it's run on
/// in `CRYSTAL_EVENT` and what the event is about in the variables every
/// plugin command finds. An event goes on its standard input, and in
/// `CRYSTAL_EVENT_JSON`; what happened, in words, in `CRYSTAL_EVENT_TEXT`.
fn command(plugin: &Id, job: &Job, socket: &Path) -> Command {
    let context = job.event.as_deref().map(Context::of_event);
    let context = context.unwrap_or_default();
    let mut command = plugins::command(plugin, &job.dir, &job.command, socket, &context);
    command.env("CRYSTAL_EVENT", job.on());
    match &job.event {
        Some(event) => {
            let json = serde_json::to_string(event).unwrap_or_default();
            command
                .env("CRYSTAL_EVENT_JSON", json)
                .env("CRYSTAL_EVENT_TEXT", event.line())
                .stdin(Stdio::piped())
        }
        None => command
            .env_remove("CRYSTAL_EVENT_JSON")
            .env_remove("CRYSTAL_EVENT_TEXT")
            .stdin(Stdio::null()),
    };
    command
}

/// Gives a hook just started its event, on a line of its own. A hook that
/// doesn't read it is fine.
fn hand_over(child: &mut std::process::Child, job: &Job) {
    if let (Some(mut stdin), Some(event)) = (child.stdin.take(), &job.event)
        && let Ok(line) = serde_json::to_string(event)
    {
        let _ = writeln!(stdin, "{line}");
    }
}

/// Runs a hook of the plugin `plugin` here, in the foreground, its output
/// going where this process's does: how `crystal plugin run --event` tries
/// a plugin's hooks out. It's neither logged nor counted towards a pause,
/// and runs as long as it takes.
pub fn run_here(
    plugin: &Id,
    dir: &Path,
    words: &[String],
    socket: &Path,
    event: &Event,
) -> Result<ExitStatus> {
    let job = Job {
        dir: dir.to_path_buf(),
        command: words.to_vec(),
        event: Some(Arc::new(event.clone())),
        timeout: plugin_manifest::TIMEOUT,
    };
    let mut child = command(plugin, &job, socket)
        .spawn()
        .with_context(|| format!("couldn't run {}", words.join(" ")))?;
    hand_over(&mut child, &job);
    Ok(child.wait()?)
}

/// Runs one of the plugin's commands, with its event on its standard input
/// and what it prints in the plugin's log, and stops it if it runs past
/// its timeout.
fn run(socket: &Path, plugin: &Id, job: &Job) -> Result<()> {
    let timeout = job.timeout;
    let name = job.on();
    plugins::log(
        socket,
        plugin,
        &format!("{name}: {}", job.command.join(" ")),
    );
    let log = plugins::open_log(socket, plugin)?;
    let mut child = command(plugin, job, socket)
        .stdout(log.try_clone()?)
        .stderr(log)
        // A process group of its own, which a handover can stop whole.
        .process_group(0)
        .spawn()?;
    let _helper = HELPERS.started(child.id());
    hand_over(&mut child, job);
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("{} on {name}: {status}", job.command.join(" "));
            }
            return Ok(());
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "{} on {name}: still running after {}s, so it was stopped",
                job.command.join(" "),
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Pauses a plugin that keeps failing, and tells the user so, and how to
/// turn it back on.
fn pause(socket: &Path, bus: &Weak<Bus>, plugin: &Id) {
    let (label, name) = (plugin.label(), &plugin.name);
    let flags = match &plugin.project {
        Some(project) => format!(
            " --project -C {}",
            shell::quote(&shell::home_relative(project))
        ),
        None => String::new(),
    };
    let text = format!(
        "the {label} plugin failed {FAILURES_TO_PAUSE} times in a row and was paused: \
         see `crystal plugin log {name}{flags}`, then turn it back on with \
         `crystal plugin enable {name}{flags}`"
    );
    plugins::log(socket, plugin, &text);
    if let Err(err) = plugins::pause(socket, plugin, &text) {
        errln!("crystal daemon: couldn't pause the {label} plugin: {err:#}");
    }
    errln!("crystal daemon: {text}");
    if let Some(bus) = bus.upgrade() {
        let paused = Event {
            project: plugin.project.clone(),
            ..Event::plugin_paused(name, &text)
        };
        bus.emit(paused);
    }
    // It's waiting on the user to look at it.
    let notice = Notice {
        session: label,
        ..Notice::of_crystal(text, None)
    };
    notify::tell(notice, socket);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Kind;
    use std::fs;

    fn job(dir: &Path, script: &str) -> Job {
        fs::write(dir.join("hook.sh"), script).unwrap();
        Job {
            dir: dir.to_path_buf(),
            command: vec!["sh".into(), "hook.sh".into()],
            event: Some(Arc::new(Event::new(Kind::SessionDone))),
            timeout: plugin_manifest::TIMEOUT,
        }
    }

    fn notes() -> Id {
        Id::own("notes")
    }

    #[test]
    fn a_hook_reads_the_event_and_its_output_goes_to_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crystal.sock");
        let job = job(
            dir.path(),
            "echo \"got $CRYSTAL_EVENT: $CRYSTAL_EVENT_TEXT\"; echo \"$CRYSTAL_EVENT_JSON\"; cat\n",
        );
        run(&socket, &notes(), &job).unwrap();
        let log = fs::read_to_string(plugins::log_path(&socket, &notes())).unwrap();
        assert!(log.contains("got session.done: -: session.done"), "{log}");
        assert_eq!(log.matches(r#""event":"session.done""#).count(), 2, "{log}");
    }

    #[test]
    fn a_startup_command_is_told_so_and_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crystal.sock");
        let mut job = job(
            dir.path(),
            "echo \"started: $CRYSTAL_EVENT in $CRYSTAL_PLUGIN_STATE_DIR\"; cat\n",
        );
        job.event = None;
        run(&socket, &notes(), &job).unwrap();
        let log = fs::read_to_string(plugins::log_path(&socket, &notes())).unwrap();
        assert!(log.contains("startup: sh hook.sh"), "{log}");
        let state = plugins::own_state_dir(&socket, &notes());
        assert!(
            log.contains(&format!("started: startup in {}", state.display())),
            "{log}"
        );
        assert!(state.is_dir());
    }

    #[test]
    fn a_hook_that_fails_or_runs_too_long_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crystal.sock");
        let err = run(&socket, &notes(), &job(dir.path(), "exit 3\n")).unwrap_err();
        assert!(err.to_string().contains("exit status: 3"), "{err:#}");
        let slow = Job {
            timeout: Duration::from_millis(100),
            ..job(dir.path(), "exec sleep 5\n")
        };
        let err = run(&socket, &notes(), &slow).unwrap_err();
        assert!(err.to_string().contains("stopped"), "{err:#}");
    }

    #[test]
    fn a_projects_plugin_hears_only_what_happens_in_its_project() {
        let about = |project: Option<&str>| Event {
            project: project.map(PathBuf::from),
            ..Event::new(Kind::SessionDone)
        };
        let lint = Id::of_project("lint", Path::new("/code/app"));
        assert!(hears(&lint, &about(Some("/code/app"))));
        assert!(!hears(&lint, &about(Some("/code/other"))));
        assert!(!hears(&lint, &about(None)));
        assert!(hears(&notes(), &about(None)));
        assert!(hears(&notes(), &about(Some("/code/other"))));
    }
}
