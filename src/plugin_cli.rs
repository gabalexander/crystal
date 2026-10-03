//! `crystal plugin`: listing plugins, switching them on and off, running
//! their actions, and installing, building, making and removing them.

use crate::client;
use crate::config::{self, Config};
use crate::env;
use crate::events::{self, Kind};
use crate::memory_cli::confirm;
use crate::plugin_hooks;
use crate::plugin_manifest::{self, Manifest};
use crate::plugins::{self, Context, Installed};
use crate::protocol::{Request, Response, SessionInfo};
use crate::shell;
use anyhow::{Context as _, Result, anyhow, bail};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Command, Stdio};

/// Prints every plugin, crystal's own first, with whether it's on.
pub fn list(socket: &Path) -> Result<()> {
    let config = Config::load()?;
    let on_off = |on: bool| if on { "on" } else { "off" }.to_string();
    let mut rows: Vec<[String; 4]> = plugins::BUILT_IN
        .iter()
        .map(|plugin| {
            [
                plugin.name.to_string(),
                on_off(plugins::enabled(&config, plugin.name)),
                "built-in".to_string(),
                plugin.description.to_string(),
            ]
        })
        .collect();
    for plugin in plugins::installed() {
        let mut state = on_off(plugins::enabled(&config, &plugin.name));
        if state == "on" && plugins::paused(socket, &plugin.name).is_some() {
            state = "paused".to_string();
        }
        let blocked = plugin.blocked();
        let row = match plugin.manifest {
            Ok(manifest) => {
                let (state, about) = match blocked {
                    Some(why) if manifest.unfit().is_some() => ("unsupported".to_string(), why),
                    Some(why) => ("unbuilt".to_string(), why),
                    None => (state, manifest.description),
                };
                [plugin.name, state, manifest.version, about]
            }
            Err(why) => [plugin.name, "broken".to_string(), "-".to_string(), why],
        };
        rows.push(row);
    }
    crate::print_table(["NAME", "STATE", "VERSION", "DESCRIPTION"], &rows);
    if plugins::enabled(&config, "notifications") && !config.notify {
        println!();
        println!("{}", NOTIFY_OFF);
    }
    Ok(())
}

/// What's said when the notifications plugin is on but the older `notify`
/// setting keeps them off.
const NOTIFY_OFF: &str = "`notify = false` in the config keeps notifications off too: \
                          take it out to hear from crystal";

/// `crystal plugin enable` and `disable`.
pub fn switch(socket: &Path, name: &str, on: bool) -> Result<()> {
    let installed = plugins::installed();
    let found = installed.iter().find(|plugin| plugin.name == name);
    if !plugins::is_built_in(name) && found.is_none() {
        bail!(
            "there's no plugin called {name} in {}: `crystal plugin` lists them",
            plugins::plugins_dir().display()
        );
    }
    if let (true, Some(plugin)) = (on, found) {
        plugins::check_can_enable(plugin, &installed)?;
    }
    plugins::set_enabled(&config::path(), socket, name, on)?;
    println!("the {name} plugin is {}", if on { "on" } else { "off" });
    if on && name == "notifications" && !Config::load()?.notify {
        println!("{NOTIFY_OFF}");
    }
    Ok(())
}

/// `crystal plugin run`: runs one of a plugin's actions here, in the
/// foreground, on `link` if it's given one, and returns its exit code.
pub fn run(
    socket: &Path,
    plugin: &str,
    action: &str,
    session: Option<String>,
    link: Option<&str>,
) -> Result<i32> {
    let found = find(plugin)?;
    plugins::ensure_enabled(&Config::load()?, plugin)?;
    if let Some(why) = found.blocked() {
        bail!("{plugin} can't run: {why}");
    }
    let manifest = manifest_of(&found)?;
    let Some(action) = manifest.action(action) else {
        let ids: Vec<&str> = manifest.actions.iter().map(|a| a.id.as_str()).collect();
        bail!(
            "{plugin} has no action {action}; its actions: {}",
            ids.join(", ")
        );
    };
    let context = Context {
        link: link.map(str::to_string),
        ..context_for(socket, session)?
    };
    let status = plugins::command(plugin, &found.dir, &action.command, socket, &context)
        .status()
        .with_context(|| format!("couldn't run {}", action.command.join(" ")))?;
    Ok(status.code().unwrap_or(1))
}

/// `crystal plugin run --link`: runs the action the plugin's first link
/// handler that takes `link` names, on it, as a Ctrl+click on it would, and
/// returns its exit code.
pub fn run_link(socket: &Path, plugin: &str, link: &str, session: Option<String>) -> Result<i32> {
    let found = find(plugin)?;
    let manifest = manifest_of(&found)?;
    let handler = (manifest.link_handlers.iter())
        .find(|handler| handler.takes(link))
        .with_context(|| format!("{plugin} has no link handler that takes {link}"))?;
    run(socket, plugin, &handler.action, session, Some(link))
}

/// `crystal plugin run --event`: runs the plugin's hooks on a made-up
/// event of the kind called `kind`, here, in the foreground, whether the
/// plugin is on or not, and returns the exit code of the first that fails,
/// or 0. The event is about the session `session` names, or else the one
/// it's run in, or else a made-up one in the current directory.
pub fn run_event(socket: &Path, plugin: &str, kind: &str, session: Option<String>) -> Result<i32> {
    let found = find(plugin)?;
    let manifest = manifest_of(&found)?;
    events::check_pattern(kind)?;
    let kind =
        Kind::named(kind).with_context(|| format!("say one event, not a pattern like `{kind}`"))?;
    let session = session_for(socket, session)?;
    let event = events::example(kind, session.as_ref(), &std::env::current_dir()?);
    let hooks: Vec<_> = plugin_hooks::hooks_on(manifest, &event).collect();
    if hooks.is_empty() {
        bail!("{plugin} has no hook on {}", kind.name());
    }
    for hook in hooks {
        let status = plugin_hooks::run_here(plugin, &found.dir, &hook.command, socket, &event)?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1));
        }
    }
    Ok(0)
}

/// What an action run from the command line is about: the session it
/// names, or else the one it's run in, or else the current directory.
fn context_for(socket: &Path, session: Option<String>) -> Result<Context> {
    match session_for(socket, session)? {
        Some(found) => Ok(Context::of_session(&found)),
        None => Ok(Context::of_dir(&std::env::current_dir()?)),
    }
}

/// The session `session` names, or else the one this runs in, if any.
fn session_for(socket: &Path, session: Option<String>) -> Result<Option<SessionInfo>> {
    let sessions = || -> Result<Vec<SessionInfo>> {
        match client::ask(socket, &Request::List, false)? {
            Some(Response::Sessions { sessions }) => Ok(sessions),
            _ => Ok(Vec::new()),
        }
    };
    if let Some(name) = session {
        let found = sessions()?.into_iter().find(|info| info.name == name);
        return found
            .map(Some)
            .with_context(|| format!("no session named {name}"));
    }
    match env::own_session_id(socket) {
        Some(id) => Ok(sessions()?.into_iter().find(|info| info.id == id)),
        None => Ok(None),
    }
}

/// `crystal plugin install`: fetches a plugin from a git repository, or
/// copies it from a directory, shows what it would run, and installs it,
/// off unless `enable` says otherwise, once the user says yes.
pub fn install(socket: &Path, source: &str, yes: bool, enable: bool) -> Result<()> {
    let dir = plugins::plugins_dir();
    fs::create_dir_all(&dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    // Fetched beside where it goes, so moving it there is one rename.
    let staging = dir.join(format!(".installing-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    let installed =
        fetch(source, &staging).and_then(|()| install_from(socket, source, &staging, yes, enable));
    let _ = fs::remove_dir_all(&staging);
    installed
}

fn fetch(source: &str, into: &Path) -> Result<()> {
    let path = Path::new(source);
    if path.is_dir() {
        return copy_dir(path, into).with_context(|| format!("couldn't copy {}", path.display()));
    }
    let status = Command::new("git")
        .args(["clone", "--quiet", "--depth", "1", "--", source])
        .arg(into)
        .status()
        .context("couldn't run git")?;
    if !status.success() {
        bail!("git couldn't clone {source}");
    }
    Ok(())
}

/// Copies the directory `from` to `to`, all but a git repository's own
/// files.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if entry.file_name() == ".git" {
            continue;
        } else if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Installs the plugin fetched into `staging`, then builds it. One whose
/// build fails stays, but off until a build works.
fn install_from(
    socket: &Path,
    source: &str,
    staging: &Path,
    yes: bool,
    enable: bool,
) -> Result<()> {
    let text = fs::read_to_string(staging.join("plugin.toml"))
        .with_context(|| format!("{source} has no plugin.toml"))?;
    let named: Manifest = toml::from_str(&text).context("in its plugin.toml")?;
    let manifest = Manifest::parse(&text, &named.name).context("in its plugin.toml")?;
    let name = manifest.name.clone();
    let target = plugins::plugins_dir().join(&name);
    if target.exists() {
        bail!("{name} is installed already: `crystal plugin remove {name}` first");
    }
    if let Some(why) = manifest.unfit() {
        bail!("{name} can't be installed: it {why}");
    }
    if let Some((key, other)) = plugins::key_taken(&manifest, &plugins::installed()) {
        bail!("{name} wants the key {key}, which the {other} plugin has");
    }

    describe(&manifest, source);
    if !yes && !confirm(&format!("Install {name}?"))? {
        println!("{name} isn't installed");
        return Ok(());
    }
    fs::rename(staging, &target)?;
    let config_dir = plugins::config_dir(&name);
    fs::create_dir_all(&config_dir)
        .with_context(|| format!("couldn't make {}", config_dir.display()))?;
    let place = shell::home_relative(&target);
    if let Err(err) = build_and_note(socket, &name, &target, &manifest) {
        bail!(
            "installed {name} in {place}, but it's off: {err:#}\n\
             once that's fixed, `crystal plugin build {name}` builds it again"
        );
    }
    if enable {
        plugins::set_enabled(&config::path(), socket, &name, true)?;
        println!("installed {name} in {place}, and it's on");
    } else {
        println!(
            "installed {name} in {place}; it's off until you run `crystal plugin enable {name}`"
        );
    }
    Ok(())
}

/// `crystal plugin build`: runs a plugin's build commands again. One that
/// fails turns the plugin off until a build works.
pub fn build(socket: &Path, name: &str) -> Result<()> {
    let plugin = find(name)?;
    let manifest = manifest_of(&plugin)?;
    if let Some(why) = manifest.unfit() {
        bail!("{name} can't be built: it {why}");
    }
    if let Err(err) = build_and_note(socket, name, &plugin.dir, manifest) {
        let config = Config::load()?;
        if plugins::enabled(&config, name) {
            plugins::set_enabled(&config::path(), socket, name, false)?;
            bail!("{err:#}\nso {name} is off now");
        }
        return Err(err);
    }
    println!("built {name}");
    Ok(())
}

/// Builds the plugin called `name`, in `dir`, and notes whether that
/// worked, for it to be kept off until a build does.
fn build_and_note(socket: &Path, name: &str, dir: &Path, manifest: &Manifest) -> Result<()> {
    let built = run_build(socket, name, dir, manifest);
    let failed = built.as_ref().err().map(|err| format!("{err:#}"));
    plugins::set_build_failed(name, failed.as_deref())?;
    built
}

/// Runs the plugin's build commands for this system in turn, from its
/// directory `dir`, each as its user would, without the variables its
/// other commands get. What they print goes to the plugin's log, and the
/// last of it into the error when one fails.
fn run_build(socket: &Path, name: &str, dir: &Path, manifest: &Manifest) -> Result<()> {
    for once in manifest.build.iter().filter(|once| once.runs_here()) {
        let words = once.command.join(" ");
        println!("building {name}: {words}");
        plugins::log(socket, name, &format!("build: {words}"));
        let log = plugins::open_log(socket, name)?;
        let start = log.metadata()?.len();
        let argv = plugins::argv(dir, &once.command);
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .status()
            .with_context(|| format!("its build couldn't run {words}"))?;
        if !status.success() {
            let printed = last_printed(&plugins::log_path(socket, name), start);
            bail!("its build failed: {words} ended with {status}{printed}");
        }
    }
    Ok(())
}

/// The last lines a build command printed, from `start` in the log at
/// `path`, set off under the line saying it failed.
fn last_printed(path: &Path, start: u64) -> String {
    const LINES: usize = 10;
    let mut text = String::new();
    let read = fs::File::open(path).and_then(|mut log| {
        log.seek(SeekFrom::Start(start))?;
        log.read_to_string(&mut text)
    });
    if read.is_err() {
        return String::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let last = &lines[lines.len().saturating_sub(LINES)..];
    last.iter().map(|line| format!("\n  {line}")).collect()
}

/// Shows what a plugin is and every command it would run, for the user to
/// read before saying yes.
fn describe(manifest: &Manifest, source: &str) {
    println!("{} {}, from {source}", manifest.name, manifest.version);
    if !manifest.description.is_empty() {
        println!("{}", manifest.description);
    }
    println!();
    println!("It runs these commands as you, from its own directory:");
    let commands = manifest.commands();
    let width = commands
        .iter()
        .map(|(what, _)| what.len())
        .max()
        .unwrap_or(0);
    for (what, words) in commands {
        let words: Vec<String> = words.iter().map(|word| shell::quote(word)).collect();
        println!("  {what:width$}  {}", words.join(" "));
    }
    println!();
}

/// `crystal plugin remove`. The plugin's settings and what it kept stay,
/// for when it's installed again.
pub fn remove(name: &str) -> Result<()> {
    let plugin = find(name)?;
    fs::remove_dir_all(&plugin.dir)
        .with_context(|| format!("couldn't remove {}", plugin.dir.display()))?;
    plugins::set_build_failed(name, None)?;
    plugins::forget(&config::path(), name)?;
    println!("removed {name}");
    Ok(())
}

/// `crystal plugin new`: a plugin to start from, with one of each thing a
/// plugin can have.
pub fn new(name: &str) -> Result<()> {
    plugin_manifest::check_name(name)?;
    let dir = plugins::plugins_dir().join(name);
    if dir.exists() {
        bail!("{} is there already", dir.display());
    }
    fs::create_dir_all(&dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    fs::create_dir_all(plugins::config_dir(name))?;
    let files = [
        ("plugin.toml", MANIFEST),
        ("hook.sh", HOOK),
        ("hello.sh", HELLO),
        ("pane.sh", PANE),
    ];
    for (file, text) in files {
        fs::write(dir.join(file), text.replace("NAME", name))?;
    }
    println!("made {name} in {}", shell::home_relative(&dir));
    println!("turn it on with `crystal plugin enable {name}`, then press X in the TUI");
    Ok(())
}

const MANIFEST: &str = r#"name = "NAME"
version = "0.1.0"
description = "What NAME does, in a line"
# min_crystal_version = "0.3.0"    # the oldest crystal it works with
# platforms = ["macos", "linux"]   # where it runs; anywhere, left out

# Run as it's installed, and again with `crystal plugin build NAME`.
# [[build]]
# command = ["npm", "ci"]

# Run once by crystal's daemon as it starts, with CRYSTAL_EVENT=startup.
# [[startup]]
# command = ["sh", "hook.sh"]

# Run from the plugins view (X in the TUI), or with
# `crystal plugin run NAME hello`. Give it a key crystal doesn't use, and
# that key runs it from the sidebar too.
[[actions]]
id = "hello"
title = "Say hello"
command = ["sh", "hello.sh"]
# key = "H"

# Run by crystal's daemon when something happens, with the event as JSON
# on standard input. `session.*` is every session event; `*`, every event.
[[events]]
on = "*"
command = ["sh", "hook.sh"]

# A program shown over the TUI's panes, with the keyboard, until it ends
# or you press Ctrl+\.
[[panes]]
id = "events"
title = "What NAME heard"
command = ["sh", "pane.sh"]

# Links a Ctrl+click in a pane hands to one of its actions rather than to
# your browser, the link in $CRYSTAL_LINK.
# [[link_handlers]]
# pattern = "^https://example\\.com/"
# action = "hello"
"#;

const HOOK: &str = r#"#!/bin/sh
# Adds each event crystal tells NAME about to events.jsonl, beside this
# script: a line of JSON each.
cat >> events.jsonl
"#;

const HELLO: &str = r#"#!/bin/sh
# Says where it was run from. Run from the TUI, what it prints goes to
# `crystal plugin log NAME`. "$CRYSTAL_BIN" is crystal, for the rest:
# "$CRYSTAL_BIN" send "$CRYSTAL_SESSION" "..." types into the session.
echo "hello from NAME: session ${CRYSTAL_SESSION:-none}, in ${CRYSTAL_WORKTREE:-?}"
"#;

const PANE: &str = r#"#!/bin/sh
# Shows the last events NAME heard, until Enter.
echo "The last events NAME heard:"
echo
if [ -f events.jsonl ]; then tail -n 20 events.jsonl; else echo "(none yet)"; fi
echo
printf 'Press Enter to close. '
read -r _
"#;

/// `crystal plugin log`.
pub fn log(socket: &Path, name: &str) -> Result<()> {
    find(name)?;
    match fs::read_to_string(plugins::log_path(socket, name)) {
        Ok(text) => print!("{text}"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            println!("{name} hasn't logged anything yet");
        }
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

/// The installed plugin called `name`, or an error that says why there's
/// none.
fn find(name: &str) -> Result<Installed> {
    if plugins::is_built_in(name) {
        bail!("{name} is one of crystal's own plugins, not one you installed");
    }
    plugins::find(name).with_context(|| {
        format!(
            "there's no plugin called {name} in {}: `crystal plugin` lists them",
            plugins::plugins_dir().display()
        )
    })
}

fn manifest_of(plugin: &Installed) -> Result<&Manifest> {
    plugin
        .manifest
        .as_ref()
        .map_err(|why| anyhow!("{}'s plugin.toml: {why}", plugin.name))
}
