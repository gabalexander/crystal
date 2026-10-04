//! `crystal integration`: crystal's hooks put in an agent's own settings,
//! for the agents crystal doesn't start itself. A `claude` or `codex` typed
//! into a session's shell then says what it's doing, and which
//! conversation it's in, so a restart picks it up again. crystal starts
//! Claude Code with hooks of its own, `--settings`, and never needs the
//! user's files for that; Codex reads hooks only from its config files, so
//! even the Codex sessions crystal starts get them only from here.
//!
//! Installing adds `crystal hook <agent> --installed`, crystal by its path,
//! to each event crystal listens to, beside the user's own hooks, which stay
//! as they were. Installing again changes nothing, but for hooks a crystal
//! at another path installed, which it replaces. Uninstalling takes out only
//! crystal's. The hook does nothing outside a crystal session, nor for an
//! agent crystal started with hooks of its own.
//!
//! Claude Code keeps its hooks in `settings.json` in its config directory,
//! Codex in `hooks.json` in its home, in the same shape. Codex runs them
//! once its config has `[features] hooks = true`, which installing makes
//! sure of, and once the user has reviewed them in Codex's `/hooks`.
//!
//! Cursor, Droid, Qoder, Qwen Code, GitHub Copilot, Devin, Kimi Code, Letta
//! Code, MastraCode, Grok and Antigravity take hooks from their own settings
//! too, each in a shape of its own: [`crate::agent_hooks`] puts crystal's
//! there and takes them out. Pi, OpenCode, Kilo Code and Hermes Agent take
//! plugins instead, which [`crate::agent_plugins`] writes.

use crate::agent_hooks::{self, Target};
use crate::agent_plugins::{self, Plugin};
use crate::agents;
use crate::shell;
use crate::skill;
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// The agents crystal can install its hooks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Agent {
    Claude,
    Codex,
    Cursor,
    Droid,
    #[value(name = "qodercli", alias = "qoder")]
    Qoder,
    Qwen,
    Copilot,
    Devin,
    Kimi,
    Letta,
    #[value(name = "mastracode")]
    MastraCode,
    Grok,
    #[value(name = "agy", alias = "antigravity")]
    Antigravity,
    Pi,
    #[value(name = "opencode")]
    OpenCode,
    Kilo,
    Hermes,
}

/// How crystal hooks an agent.
enum Way {
    /// Claude Code's and Codex's hooks, this module's own.
    Own,
    /// Hooks in its own settings, [`crate::agent_hooks`]'.
    Hooks(&'static Target),
    /// A plugin, [`crate::agent_plugins`]'.
    Plugin(&'static Plugin),
}

/// How long a hook may take, in seconds, as crystal's own Claude Code hooks
/// do. Codex gives an `Interrupt` hook 3 at most.
const TIMEOUT: u64 = 5;
const INTERRUPT_TIMEOUT: u64 = 3;

/// How an agent's hooks stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// crystal's hook, by this crystal's path, is on each event.
    Installed,
    /// Some of crystal's hooks are there, but not all, or by another path.
    OutOfDate,
    NotInstalled,
}

impl Standing {
    pub fn word(self) -> &'static str {
        match self {
            Standing::Installed => "installed",
            Standing::OutOfDate => "out of date",
            Standing::NotInstalled => "not installed",
        }
    }
}

impl Agent {
    pub const ALL: [Agent; 17] = [
        Agent::Claude,
        Agent::Codex,
        Agent::Cursor,
        Agent::Droid,
        Agent::Qoder,
        Agent::Qwen,
        Agent::Copilot,
        Agent::Devin,
        Agent::Kimi,
        Agent::Letta,
        Agent::MastraCode,
        Agent::Grok,
        Agent::Antigravity,
        Agent::Pi,
        Agent::OpenCode,
        Agent::Kilo,
        Agent::Hermes,
    ];

    /// Its program, which is how `crystal hook` knows it.
    pub fn program(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Cursor => "cursor",
            Agent::Droid => "droid",
            Agent::Qoder => "qodercli",
            Agent::Qwen => "qwen",
            Agent::Copilot => "copilot",
            Agent::Devin => "devin",
            Agent::Kimi => "kimi",
            Agent::Letta => "letta",
            Agent::MastraCode => "mastracode",
            Agent::Grok => "grok",
            Agent::Antigravity => "agy",
            Agent::Pi => "pi",
            Agent::OpenCode => "opencode",
            Agent::Kilo => "kilo",
            Agent::Hermes => "hermes",
        }
    }

    /// The agent whose program, or other name in its rules, is `program`.
    pub fn of_program(program: &str) -> Option<Agent> {
        let id = crate::agent_rules::bundled_registry()
            .find(program)
            .map_or(program, |rules| rules.id.as_str());
        Agent::ALL.into_iter().find(|agent| agent.program() == id)
    }

    /// Its name, as people know it.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Cursor => "Cursor",
            Agent::Droid => "Droid",
            Agent::Qoder => "Qoder",
            Agent::Qwen => "Qwen Code",
            Agent::Copilot => "GitHub Copilot",
            Agent::Devin => "Devin",
            Agent::Kimi => "Kimi Code",
            Agent::Letta => "Letta Code",
            Agent::MastraCode => "MastraCode",
            Agent::Grok => "Grok",
            Agent::Antigravity => "Antigravity",
            Agent::Pi => "Pi",
            Agent::OpenCode => "OpenCode",
            Agent::Kilo => "Kilo Code",
            Agent::Hermes => "Hermes Agent",
        }
    }

    /// How crystal hooks it: Claude Code and Codex by this module itself,
    /// the agents that take plugins by [`crate::agent_plugins`], and the
    /// rest by [`crate::agent_hooks`].
    fn way(self) -> Way {
        if matches!(self, Agent::Claude | Agent::Codex) {
            return Way::Own;
        }
        if let Some(plugin) = agent_plugins::plugin(self.program()) {
            return Way::Plugin(plugin);
        }
        match agent_hooks::target(self.program()) {
            Some(target) => Way::Hooks(target),
            None => unreachable!("{} has a target", self.program()),
        }
    }

    /// The hook events crystal listens to, for Claude Code and Codex.
    fn events(self) -> &'static [&'static str] {
        match self {
            Agent::Codex => agents::CODEX_HOOK_EVENTS,
            _ => agents::CLAUDE_HOOK_EVENTS,
        }
    }

    /// Where the agent keeps its settings, as the environment says:
    /// `$CLAUDE_CONFIG_DIR` or `~/.claude`, `$CODEX_HOME` or `~/.codex`.
    fn dir(self) -> Result<PathBuf> {
        match self.way() {
            Way::Hooks(target) => return Ok(target.dir()),
            Way::Plugin(plugin) => return Ok(plugin.dir()),
            Way::Own => {}
        }
        let home = std::env::var_os("HOME");
        let dir = match self {
            Agent::Codex => codex_home(std::env::var_os("CODEX_HOME"), home),
            _ => skill::claude_config_dir(std::env::var_os("CLAUDE_CONFIG_DIR"), home),
        };
        dir.with_context(|| {
            format!(
                "can't tell where {} keeps its settings: HOME isn't set",
                self.name()
            )
        })
    }

    /// The file it keeps its hooks in, in `dir`.
    fn hooks_file(self, dir: &Path) -> PathBuf {
        match (self, self.way()) {
            (_, Way::Hooks(target)) => target.file(dir),
            (_, Way::Plugin(plugin)) => plugin.file(dir),
            (Agent::Codex, Way::Own) => dir.join("hooks.json"),
            _ => dir.join("settings.json"),
        }
    }

    /// The end of the command of a hook `crystal integration` installed,
    /// whatever crystal's path.
    fn ours(self) -> String {
        format!(" hook {} --installed", self.program())
    }
}

/// Codex's home: `$CODEX_HOME`, as Codex itself reads it, or `~/.codex`.
fn codex_home(from_env: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    match from_env {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => home.map(|home| PathBuf::from(home).join(".codex")),
    }
}

/// Each agent installed here, by whether its directory is there, with how
/// its hooks stand for `crystal` at its path: what the settings view lists.
/// One whose files can't be read is left out.
pub fn here(crystal: &Path) -> Vec<(Agent, Standing)> {
    Agent::ALL
        .into_iter()
        .filter(|agent| agent.dir().is_ok_and(|dir| dir.is_dir()))
        .filter_map(|agent| Some((agent, standing_of(agent, crystal).ok()?)))
        .collect()
}

/// The agents to work on: `agent`, or every one installed here, by whether
/// its directory is there.
pub fn chosen(agent: Option<Agent>) -> Result<Vec<Agent>> {
    if let Some(agent) = agent {
        return Ok(vec![agent]);
    }
    let mut found = Vec::new();
    for agent in Agent::ALL {
        if agent.dir()?.is_dir() {
            found.push(agent);
        }
    }
    if found.is_empty() {
        bail!("none of the agents crystal can hook is installed here: install one first");
    }
    Ok(found)
}

/// Installs crystal's hooks for `agent`, run by `crystal`, the path of this
/// program, and says what it did.
pub fn install(agent: Agent, crystal: &Path) -> Result<Vec<String>> {
    let dir = agent.dir()?;
    if let Way::Plugin(plugin) = agent.way() {
        let changed = plugin.install(&dir, crystal)?;
        if changed.is_empty() {
            return Ok(vec![format!(
                "{}: crystal's plugin is in {} already",
                agent.program(),
                shell::home_relative(&plugin.file(&dir))
            )]);
        }
        let mut said: Vec<String> = changed
            .iter()
            .map(|path| {
                let path = shell::home_relative(path);
                format!("{}: added crystal's plugin to {path}", agent.program())
            })
            .collect();
        said.push(format!(
            "{}: {} loads it as it starts: start again any that's running",
            agent.program(),
            agent.name()
        ));
        return Ok(said);
    }
    if let Way::Hooks(target) = agent.way() {
        let changed = target.install(&dir, crystal)?;
        if changed.is_empty() {
            return Ok(vec![format!(
                "{}: crystal's hooks are in {} already",
                agent.program(),
                agent.hooks_file(&dir).display()
            )]);
        }
        return Ok(changed
            .iter()
            .map(|path| {
                let path = shell::home_relative(path);
                format!("{}: added crystal's hooks to {path}", agent.program())
            })
            .collect());
    }
    if !dir.is_dir() {
        bail!(
            "{}'s directory isn't at {}; install {} first",
            agent.name(),
            dir.display(),
            agent.name()
        );
    }
    let file = agent.hooks_file(&dir);
    let command = agents::hook_command(crystal, agent.program(), true);
    let settings = read_json(&file)?;
    let wanted = with_hooks(settings.clone(), agent, &command)?;
    let mut said = Vec::new();
    if wanted == settings {
        said.push(format!(
            "{}: crystal's hooks are in {} already",
            agent.program(),
            file.display()
        ));
    } else {
        write_json(&file, &wanted)?;
        said.push(format!(
            "{}: added crystal's hooks to {}",
            agent.program(),
            file.display()
        ));
    }
    if agent == Agent::Codex {
        let config = dir.join("config.toml");
        let text = read_text(&config)?;
        let on = with_codex_hooks_on(&text)
            .with_context(|| format!("couldn't read {}", config.display()))?;
        if on != text {
            write_atomically(&config, &on)?;
            said.push(format!("codex: turned hooks on in {}", config.display()));
        }
        said.push(
            "codex: Codex runs them once you've reviewed them: /hooks in Codex, or its question as it starts".into(),
        );
    }
    Ok(said)
}

/// Takes crystal's hooks out of `agent`'s settings, and says what it did.
/// Codex's hooks stay on in its config: other hooks may need them.
pub fn uninstall(agent: Agent) -> Result<String> {
    let dir = agent.dir()?;
    let file = agent.hooks_file(&dir);
    if let Way::Plugin(plugin) = agent.way() {
        let said = match plugin.uninstall(&dir)?.is_empty() {
            true => "crystal's plugin isn't in",
            false => "took crystal's plugin out of",
        };
        return Ok(format!("{}: {said} {}", agent.program(), dir.display()));
    }
    if let Way::Hooks(target) = agent.way() {
        let said = match target.uninstall(&dir)?.is_empty() {
            true => "there are no crystal hooks in",
            false => "took crystal's hooks out of",
        };
        return Ok(format!("{}: {said} {}", agent.program(), file.display()));
    }
    if !file.is_file() {
        return Ok(format!(
            "{}: {} isn't there",
            agent.program(),
            file.display()
        ));
    }
    let (left, removed) = without_hooks(read_json(&file)?, agent);
    if !removed {
        return Ok(format!(
            "{}: there are no crystal hooks in {}",
            agent.program(),
            file.display()
        ));
    }
    write_json(&file, &left)?;
    Ok(format!(
        "{}: took crystal's hooks out of {}",
        agent.program(),
        file.display()
    ))
}

/// How `agent`'s hooks stand, a line: its program, its standing and its
/// file.
pub fn status(agent: Agent, crystal: &Path) -> Result<String> {
    let file = agent.hooks_file(&agent.dir()?);
    let standing = standing_of(agent, crystal)?;
    Ok(format!(
        "{}  {}  {}",
        agent.program(),
        standing.word(),
        file.display()
    ))
}

/// How `agent`'s hooks stand, for `crystal` at its path.
pub fn standing_of(agent: Agent, crystal: &Path) -> Result<Standing> {
    let dir = agent.dir()?;
    match agent.way() {
        Way::Hooks(target) => return Ok(target.standing(&dir, crystal)),
        Way::Plugin(plugin) => return Ok(plugin.standing(&dir, crystal)),
        Way::Own => {}
    }
    let file = agent.hooks_file(&dir);
    let command = agents::hook_command(crystal, agent.program(), true);
    Ok(standing(&read_json(&file)?, agent, &command))
}

/// `settings` with crystal's hook, `command`, on each of `agent`'s events:
/// a matcher group of its own, with no matcher, after the user's. An event
/// that has it already keeps it where it is. crystal's hooks with another
/// command, an earlier crystal's at another path, are taken out first.
pub fn with_hooks(settings: Value, agent: Agent, command: &str) -> Result<Value> {
    let (mut settings, _) = without_hooks_but(settings, agent, Some(command));
    let Some(object) = settings.as_object_mut() else {
        bail!("the settings aren't a JSON object");
    };
    let hooks = object.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        bail!("the settings' `hooks` isn't an object");
    };
    for event in agent.events() {
        let groups = hooks.entry(*event).or_insert_with(|| json!([]));
        let Some(groups) = groups.as_array_mut() else {
            bail!("the settings' hooks for {event} aren't a list");
        };
        let there = groups
            .iter()
            .any(|group| commands(group).any(|hook| hook["command"].as_str() == Some(command)));
        if !there {
            let timeout = match *event {
                "Interrupt" => INTERRUPT_TIMEOUT,
                _ => TIMEOUT,
            };
            groups.push(json!({
                "hooks": [{ "type": "command", "command": command, "timeout": timeout }]
            }));
        }
    }
    Ok(settings)
}

/// `settings` without crystal's hooks for `agent`, and whether there were
/// any. A matcher group left empty goes, and so does an event left with
/// none, and `hooks` left with no events.
pub fn without_hooks(settings: Value, agent: Agent) -> (Value, bool) {
    without_hooks_but(settings, agent, None)
}

/// `settings` without crystal's hooks for `agent`, but those that run
/// `keep`, and whether any went.
fn without_hooks_but(mut settings: Value, agent: Agent, keep: Option<&str>) -> (Value, bool) {
    let ours = agent.ours();
    let goes = |hook: &Value| {
        let command = hook["command"].as_str().unwrap_or_default();
        hook["type"] == "command" && command.ends_with(&ours) && Some(command) != keep
    };
    let mut removed = false;
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return (settings, false);
    };
    let mut emptied = Vec::new();
    for (event, groups) in hooks.iter_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        let had = groups.len();
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = list.len();
                list.retain(|hook| !goes(hook));
                removed |= list.len() != before;
            }
        }
        groups.retain(|group| {
            group["hooks"]
                .as_array()
                .is_none_or(|list| !list.is_empty())
        });
        if groups.is_empty() && had > 0 {
            emptied.push(event.clone());
        }
    }
    for event in emptied {
        hooks.shift_remove(&event);
    }
    if removed && hooks.is_empty() {
        settings
            .as_object_mut()
            .map(|object| object.shift_remove("hooks"));
    }
    (settings, removed)
}

/// How crystal's hooks for `agent` stand in `settings`, `command` being
/// what this crystal's run.
pub fn standing(settings: &Value, agent: Agent, command: &str) -> Standing {
    let ours = agent.ours();
    let hooks = &settings["hooks"];
    let mut any = false;
    let mut all = true;
    for event in agent.events() {
        let groups = hooks[*event]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut this = false;
        for hook in groups.iter().flat_map(commands) {
            let run = hook["command"].as_str().unwrap_or_default();
            any |= run.ends_with(&ours);
            this |= run == command;
        }
        all &= this;
    }
    match (any, all) {
        (true, true) => Standing::Installed,
        (true, false) => Standing::OutOfDate,
        (false, _) => Standing::NotInstalled,
    }
}

/// A matcher group's command hooks.
fn commands(group: &Value) -> impl Iterator<Item = &Value> {
    let list = group["hooks"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    list.iter().filter(|hook| hook["type"] == "command")
}

/// Codex's config, `text`, with its hooks on: `hooks = true` in its
/// `[features]`. The rest stays as it was, comments and all.
pub fn with_codex_hooks_on(text: &str) -> Result<String> {
    let mut document: toml_edit::DocumentMut = text.parse()?;
    let features = document.entry("features").or_insert_with(toml_edit::table);
    let features = features
        .as_table_like_mut()
        .context("its `features` isn't a table")?;
    if features.get("hooks").and_then(toml_edit::Item::as_bool) != Some(true) {
        features.insert("hooks", toml_edit::value(true));
    }
    Ok(document.to_string())
}

/// The JSON object in `file`, or an empty one when there's no file yet.
fn read_json(file: &Path) -> Result<Value> {
    let text = read_text(file)?;
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(&text).with_context(|| format!("couldn't read {}", file.display()))
}

/// What's in `file`, or nothing when it isn't there.
fn read_text(file: &Path) -> Result<String> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("couldn't read {}", file.display())),
    }
}

/// Writes `value` to `file` the way the agents write it themselves:
/// indented by two, with a newline at the end.
fn write_json(file: &Path, value: &Value) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    write_atomically(file, &text)
}

/// Writes `text` to `path` in one go: to a file beside it, then moved into
/// place, so an agent never reads half of it. A symbolic link stays one:
/// the file it leads to is written. A file that was there keeps its
/// permissions.
fn write_atomically(path: &Path, text: &str) -> Result<()> {
    let target = followed(path);
    let unfinished = target.with_extension(format!(
        "{}crystal-saving",
        target
            .extension()
            .map(|extension| format!("{}.", extension.to_string_lossy()))
            .unwrap_or_default()
    ));
    fs::write(&unfinished, text)
        .with_context(|| format!("couldn't write {}", unfinished.display()))?;
    if let Ok(meta) = fs::metadata(&target) {
        fs::set_permissions(&unfinished, meta.permissions())?;
    }
    if let Err(err) = fs::rename(&unfinished, &target) {
        let _ = fs::remove_file(&unfinished);
        return Err(err).with_context(|| format!("couldn't write {}", target.display()));
    }
    Ok(())
}

/// Where `path` leads, through any symbolic links, even to a file that
/// isn't there yet.
fn followed(path: &Path) -> PathBuf {
    let mut path = path.to_path_buf();
    // Links that lead round in a circle end somewhere.
    for _ in 0..32 {
        let Ok(link) = fs::read_link(&path) else {
            break;
        };
        path = match path.parent() {
            Some(dir) if link.is_relative() => dir.join(link),
            _ => link,
        };
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMAND: &str = "/bin/crystal hook claude --installed";

    fn user_settings() -> Value {
        json!({
            "model": "opus",
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": "say done" }] }],
                "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "lint" }] }]
            },
            "permissions": { "allow": ["Bash(ls:*)"] }
        })
    }

    fn crystal_commands(settings: &Value, event: &str) -> Vec<String> {
        let groups = settings["hooks"][event]
            .as_array()
            .cloned()
            .unwrap_or_default();
        groups
            .iter()
            .flat_map(|group| commands(group).cloned().collect::<Vec<_>>())
            .filter_map(|hook| hook["command"].as_str().map(String::from))
            .filter(|command| command.ends_with(" --installed"))
            .collect()
    }

    #[test]
    fn installing_adds_crystal_s_hook_to_each_event_and_keeps_the_rest() {
        let settings = with_hooks(user_settings(), Agent::Claude, COMMAND).unwrap();
        for event in agents::CLAUDE_HOOK_EVENTS {
            assert_eq!(crystal_commands(&settings, event), [COMMAND], "{event}");
        }
        assert_eq!(settings["model"], "opus");
        assert_eq!(settings["permissions"], user_settings()["permissions"]);
        // The user's own hooks come first, and stay as they were.
        assert_eq!(
            settings["hooks"]["Stop"][0],
            user_settings()["hooks"]["Stop"][0]
        );
        assert_eq!(
            settings["hooks"]["PreToolUse"],
            user_settings()["hooks"]["PreToolUse"]
        );
        let ours = &settings["hooks"]["Stop"][1];
        assert_eq!(ours["hooks"][0]["timeout"], 5);
        assert!(ours.get("matcher").is_none());
    }

    #[test]
    fn installing_twice_changes_nothing() {
        let once = with_hooks(user_settings(), Agent::Claude, COMMAND).unwrap();
        let twice = with_hooks(once.clone(), Agent::Claude, COMMAND).unwrap();
        assert_eq!(once, twice);
        assert_eq!(
            serde_json::to_string_pretty(&once).unwrap(),
            serde_json::to_string_pretty(&twice).unwrap()
        );
    }

    #[test]
    fn installing_keeps_the_order_of_the_keys() {
        let settings = with_hooks(user_settings(), Agent::Claude, COMMAND).unwrap();
        let keys: Vec<&String> = settings.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "hooks", "permissions"]);
    }

    #[test]
    fn an_earlier_crystal_s_hooks_at_another_path_are_replaced() {
        let old = "/old/crystal hook claude --installed";
        let earlier = with_hooks(user_settings(), Agent::Claude, old).unwrap();
        assert_eq!(
            standing(&earlier, Agent::Claude, COMMAND),
            Standing::OutOfDate
        );
        let now = with_hooks(earlier, Agent::Claude, COMMAND).unwrap();
        for event in agents::CLAUDE_HOOK_EVENTS {
            assert_eq!(crystal_commands(&now, event), [COMMAND], "{event}");
        }
        // crystal's own `--settings` hook isn't one installed.
        let own = json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "/x/crystal hook claude"}]}]}});
        let (kept, removed) = without_hooks(own.clone(), Agent::Claude);
        assert!(!removed);
        assert_eq!(kept, own);
    }

    #[test]
    fn uninstalling_takes_out_only_crystal_s_hooks() {
        let installed = with_hooks(user_settings(), Agent::Claude, COMMAND).unwrap();
        let (left, removed) = without_hooks(installed, Agent::Claude);
        assert!(removed);
        assert_eq!(left, user_settings());

        let (left, removed) = without_hooks(user_settings(), Agent::Claude);
        assert!(!removed);
        assert_eq!(left, user_settings());

        // Settings that had no hooks have none again.
        let bare = json!({"model": "opus"});
        let installed = with_hooks(bare.clone(), Agent::Claude, COMMAND).unwrap();
        assert_eq!(without_hooks(installed, Agent::Claude).0, bare);
    }

    #[test]
    fn a_group_shared_with_the_user_s_hooks_keeps_them() {
        let shared = json!({"hooks": {"Stop": [{"hooks": [
            {"type": "command", "command": "say done"},
            {"type": "command", "command": COMMAND}
        ]}]}});
        let (left, removed) = without_hooks(shared, Agent::Claude);
        assert!(removed);
        assert_eq!(
            left,
            json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]}})
        );
    }

    #[test]
    fn the_standing_says_whether_every_event_has_this_crystal_s_hook() {
        assert_eq!(
            standing(&user_settings(), Agent::Claude, COMMAND),
            Standing::NotInstalled
        );
        assert_eq!(
            standing(&json!({}), Agent::Claude, COMMAND),
            Standing::NotInstalled
        );
        let installed = with_hooks(user_settings(), Agent::Claude, COMMAND).unwrap();
        assert_eq!(
            standing(&installed, Agent::Claude, COMMAND),
            Standing::Installed
        );
        let mut missing = installed.clone();
        missing["hooks"]
            .as_object_mut()
            .unwrap()
            .shift_remove("SubagentStop");
        assert_eq!(
            standing(&missing, Agent::Claude, COMMAND),
            Standing::OutOfDate
        );
    }

    #[test]
    fn codex_gets_its_own_events_and_a_shorter_interrupt() {
        let command = "/bin/crystal hook codex --installed";
        let hooks = with_hooks(json!({}), Agent::Codex, command).unwrap();
        let events: Vec<&String> = hooks["hooks"].as_object().unwrap().keys().collect();
        assert_eq!(events, agents::CODEX_HOOK_EVENTS);
        assert_eq!(hooks["hooks"]["Interrupt"][0]["hooks"][0]["timeout"], 3);
        assert_eq!(hooks["hooks"]["Stop"][0]["hooks"][0]["timeout"], 5);
        // Claude's hooks aren't Codex's.
        assert!(!without_hooks(hooks, Agent::Claude).1);
    }

    #[test]
    fn codex_s_hooks_are_turned_on_and_the_rest_of_its_config_kept() {
        let config = "# mine\nmodel = \"gpt-5\"\n\n[features]\n# on\nweb = true\n";
        let on = with_codex_hooks_on(config).unwrap();
        assert_eq!(
            on,
            "# mine\nmodel = \"gpt-5\"\n\n[features]\n# on\nweb = true\nhooks = true\n"
        );
        assert_eq!(with_codex_hooks_on(&on).unwrap(), on);
        assert_eq!(
            with_codex_hooks_on("").unwrap(),
            "[features]\nhooks = true\n"
        );
        let off = "[features]\nhooks = false\n";
        assert_eq!(
            with_codex_hooks_on(off).unwrap(),
            "[features]\nhooks = true\n"
        );
        assert!(with_codex_hooks_on("features = 3\n").is_err());
    }

    #[test]
    fn a_symbolic_link_is_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.json");
        let link = dir.path().join("settings.json");
        fs::write(&real, "{}").unwrap();
        std::os::unix::fs::symlink("real.json", &link).unwrap();
        write_json(&link, &json!({"a": 1})).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "{\n  \"a\": 1\n}\n");
        let left: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 2, "nothing is left behind");
    }

    #[test]
    fn every_agent_is_hooked_one_way_or_another_by_its_names() {
        use clap::ValueEnum;
        for agent in Agent::ALL {
            // No agent is left without a way to hook it.
            let _ = agent.way();
            assert_eq!(Agent::of_program(agent.program()), Some(agent));
            assert_eq!(Agent::from_str(agent.program(), false), Ok(agent));
        }
        assert_eq!(Agent::of_program("cursor-agent"), Some(Agent::Cursor));
        assert_eq!(Agent::of_program("antigravity"), Some(Agent::Antigravity));
        assert_eq!(
            Agent::from_str("antigravity", false),
            Ok(Agent::Antigravity)
        );
    }

    #[test]
    fn codex_s_home_is_codex_home_or_under_home() {
        assert_eq!(
            codex_home(Some("/c".into()), Some("/h".into())),
            Some(PathBuf::from("/c"))
        );
        assert_eq!(
            codex_home(Some("".into()), Some("/h".into())),
            Some(PathBuf::from("/h/.codex"))
        );
        assert_eq!(codex_home(None, None), None);
    }
}
