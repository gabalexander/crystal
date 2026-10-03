//! `crystal plugin`: listing plugins, switching them on and off, running
//! their actions, and installing, making and removing them.

use crate::client;
use crate::config::{self, Config};
use crate::env;
use crate::memory_cli::confirm;
use crate::plugin_manifest::{self, Manifest};
use crate::plugins::{self, Context, Installed};
use crate::protocol::{Request, Response, SessionInfo};
use crate::shell;
use anyhow::{Context as _, Result, anyhow, bail};
use std::fs;
use std::path::Path;
use std::process::Command;

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
        let row = match plugin.manifest {
            Ok(manifest) => [plugin.name, state, manifest.version, manifest.description],
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
        let manifest = manifest_of(plugin)?;
        if let Some((key, other)) = plugins::key_taken(manifest, &installed) {
            bail!("{name} wants the key {key}, which the {other} plugin has");
        }
    }
    plugins::set_enabled(&config::path(), socket, name, on)?;
    println!("the {name} plugin is {}", if on { "on" } else { "off" });
    if on && name == "notifications" && !Config::load()?.notify {
        println!("{NOTIFY_OFF}");
    }
    Ok(())
}

/// `crystal plugin run`: runs one of a plugin's actions here, in the
/// foreground, and returns its exit code.
pub fn run(socket: &Path, plugin: &str, action: &str, session: Option<String>) -> Result<i32> {
    let found = find(plugin)?;
    plugins::ensure_enabled(&Config::load()?, plugin)?;
    let manifest = manifest_of(&found)?;
    let Some(action) = manifest
        .actions
        .iter()
        .find(|candidate| candidate.id == action)
    else {
        let ids: Vec<&str> = manifest.actions.iter().map(|a| a.id.as_str()).collect();
        bail!(
            "{plugin} has no action {action}; its actions: {}",
            ids.join(", ")
        );
    };
    let context = context_for(socket, session)?;
    let status = plugins::command(&found.dir, &action.command, socket, &context)
        .status()
        .with_context(|| format!("couldn't run {}", action.command.join(" ")))?;
    Ok(status.code().unwrap_or(1))
}

/// What an action run from the command line is about: the session it
/// names, or else the one it's run in, or else the current directory.
fn context_for(socket: &Path, session: Option<String>) -> Result<Context> {
    let sessions = || -> Result<Vec<SessionInfo>> {
        match client::ask(socket, &Request::List, false)? {
            Some(Response::Sessions { sessions }) => Ok(sessions),
            _ => Ok(Vec::new()),
        }
    };
    if let Some(name) = session {
        let found = sessions()?.into_iter().find(|info| info.name == name);
        let found = found.with_context(|| format!("no session named {name}"))?;
        return Ok(Context::of_session(&found));
    }
    if let Some(id) = env::own_session_id(socket)
        && let Some(found) = sessions()?.into_iter().find(|info| info.id == id)
    {
        return Ok(Context::of_session(&found));
    }
    Ok(Context::of_dir(&std::env::current_dir()?))
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

/// Installs the plugin fetched into `staging`.
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
    if let Some((key, other)) = plugins::key_taken(&manifest, &plugins::installed()) {
        bail!("{name} wants the key {key}, which the {other} plugin has");
    }

    describe(&manifest, source);
    if !yes && !confirm(&format!("Install {name}?"))? {
        println!("{name} isn't installed");
        return Ok(());
    }
    fs::rename(staging, &target)?;
    if enable {
        plugins::set_enabled(&config::path(), socket, &name, true)?;
        println!("installed {name} in {}, and it's on", target.display());
    } else {
        println!(
            "installed {name} in {}; it's off until you run `crystal plugin enable {name}`",
            target.display()
        );
    }
    Ok(())
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

/// `crystal plugin remove`.
pub fn remove(name: &str) -> Result<()> {
    let plugin = find(name)?;
    fs::remove_dir_all(&plugin.dir)
        .with_context(|| format!("couldn't remove {}", plugin.dir.display()))?;
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
