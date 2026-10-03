//! The daemon's side of plugins' `[[events]]`: when something happens in
//! crystal, the hooks of the plugins that asked for it run, each with the
//! event as JSON on its standard input and its name in `CRYSTAL_EVENT`.
//! They hear it from the daemon's [`Bus`], like any other subscriber.
//!
//! A plugin's hooks run one at a time, in the order things happened, on a
//! thread of the plugin's own, so a slow plugin holds up neither the daemon
//! nor the others. What a hook prints goes to the plugin's log. A hook that
//! runs past [`TIMEOUT`] is stopped, and a plugin whose hooks fail
//! [`FAILURES_TO_PAUSE`] times in a row is paused, with a notice and a
//! `plugin.paused` event, until the user turns it on again.

use crate::config::Config;
use crate::event_log::Bus;
use crate::events::{Event, Filter};
use crate::notify::{self, Notice};
use crate::plugin_manifest::{self, Manifest};
use crate::plugins::{self, Context};
use crate::protocol::Activity;
use anyhow::{Context as _, Result, bail};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

/// How long a hook may run before it's stopped.
const TIMEOUT: Duration = Duration::from_secs(30);

/// How many times in a row a plugin's hooks may fail before it's paused.
const FAILURES_TO_PAUSE: u32 = 5;

/// Hands every event on `bus` to the hooks of the plugins that listen for
/// it, from a thread of its own, for as long as the daemon runs.
pub fn follow(bus: &Arc<Bus>, socket: &Path) {
    let hooks = Hooks::new(socket, Arc::downgrade(bus));
    let bus = Arc::downgrade(bus);
    thread::spawn(move || {
        while let Some(subscription) = bus.upgrade().map(|bus| bus.subscribe(Filter::default())) {
            for event in subscription.feed {
                hooks.tell(&event);
            }
            // Dropped for falling behind: what was missed is lost, and the
            // hooks hear what happens from now on.
            eprintln!("crystal daemon: the plugins fell behind, and missed events");
        }
    });
}

/// The hooks of every plugin, run off the daemon's own threads.
struct Hooks {
    socket: PathBuf,
    /// Where a plugin paused for failing says so.
    bus: Weak<Bus>,
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
    fn new(socket: &Path, bus: Weak<Bus>) -> Hooks {
        Hooks {
            socket: socket.to_path_buf(),
            bus,
            queues: Mutex::default(),
        }
    }

    /// Hands `event` to the hooks of every plugin that's on and listens
    /// for it, to run in turn after the plugin's others.
    fn tell(&self, event: &Arc<Event>) {
        let config = Config::load().unwrap_or_default();
        for (dir, manifest) in plugins::running(&config, &self.socket) {
            for hook in hooks_on(&manifest, event) {
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
            let bus = self.bus.clone();
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

/// A hook's `command`, from the plugin's directory `dir`, with `event`'s
/// name in `CRYSTAL_EVENT` and what it's about in the variables every
/// plugin command finds. The event goes on its standard input.
fn command(dir: &Path, words: &[String], socket: &Path, event: &Event) -> Command {
    let mut command = plugins::command(dir, words, socket, &Context::of_event(event));
    command
        .env("CRYSTAL_EVENT", event.kind.name())
        .stdin(Stdio::piped());
    command
}

/// Gives a hook just started the event, on a line of its own. A hook that
/// doesn't read it is fine.
fn hand_over(child: &mut std::process::Child, event: &Event) {
    if let Some(mut stdin) = child.stdin.take()
        && let Ok(line) = serde_json::to_string(event)
    {
        let _ = writeln!(stdin, "{line}");
    }
}

/// Runs a hook here, in the foreground, its output going where this
/// process's does: how `crystal plugin run --event` tries a plugin's hooks
/// out. It's neither logged nor counted towards a pause.
pub fn run_here(dir: &Path, words: &[String], socket: &Path, event: &Event) -> Result<ExitStatus> {
    let mut child = command(dir, words, socket, event)
        .spawn()
        .with_context(|| format!("couldn't run {}", words.join(" ")))?;
    hand_over(&mut child, event);
    Ok(child.wait()?)
}

/// Runs one hook, with the event on its standard input and what it prints
/// in the plugin's log, and stops it if it runs past `timeout`.
fn run(socket: &Path, plugin: &str, job: &Job, timeout: Duration) -> Result<()> {
    let event = &job.event;
    let name = event.kind.name();
    plugins::log(
        socket,
        plugin,
        &format!("{name}: {}", job.command.join(" ")),
    );
    let log = plugins::open_log(socket, plugin)?;
    let mut child = command(&job.dir, &job.command, socket, event)
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    hand_over(&mut child, event);
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
fn pause(socket: &Path, bus: &Weak<Bus>, plugin: &str) {
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
    if let Some(bus) = bus.upgrade() {
        bus.emit(Event::plugin_paused(plugin, &text));
    }
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
    use crate::events::Kind;
    use std::fs;

    fn job(dir: &Path, script: &str) -> Job {
        fs::write(dir.join("hook.sh"), script).unwrap();
        Job {
            dir: dir.to_path_buf(),
            command: vec!["sh".into(), "hook.sh".into()],
            event: Arc::new(Event::new(Kind::SessionDone)),
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
        assert!(log.contains(r#""event":"session.done""#), "{log}");
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
