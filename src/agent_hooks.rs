//! crystal's hooks in the own settings of agents that take hooks from
//! nowhere else, beyond Claude Code and Codex, which [`crate::integration`]
//! handles itself: Cursor, Droid, Qoder, Qwen Code, GitHub Copilot, Devin,
//! Kimi Code, Letta Code, MastraCode, Grok and Antigravity. Put there when
//! the user asks, with `crystal integration install <agent>`, and taken out
//! with `uninstall`.
//!
//! The hook runs `crystal hook <agent>` only inside a crystal session, so
//! the agent run anywhere else goes on as before, and a hook that fails
//! never holds it up. crystal finds its own hooks again by their command,
//! so taking them out leaves the user's alone. A JSON settings file is
//! written again whole, as formatted JSON, its keys in order; Kimi's TOML
//! keeps its comments and layout.
//!
//! The agents and their files are as herdr installs its hooks for them.
//! Each gets the events that say what it's doing and which conversation
//! it's in, as far as it has them, and as far as they can be trusted: an
//! agent whose hooks miss a turn cut short gets only those that name its
//! conversation, and its screen says the rest.

use crate::agent_rules;
use crate::shell;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// An agent crystal can add hooks to, and how.
#[derive(Debug)]
pub struct Target {
    /// The agent, by its id in its rules.
    pub agent: &'static str,
    /// The variables that move its settings directory, the first one set
    /// winning, each with where the directory is in the one it names.
    env: &'static [(&'static str, &'static str)],
    /// Its settings directory, in the home directory.
    home: &'static str,
    /// The file in that directory its hooks go in.
    file: &'static str,
    shape: Shape,
    place: Place,
    /// The hooks crystal adds.
    hooks: &'static [Hook],
    /// Whether the file is crystal's alone, which goes when its hooks do:
    /// Grok reads every file in its `hooks` directory.
    own_file: bool,
    /// Whether crystal makes the settings directory when it isn't there,
    /// as herdr does for MastraCode, rather than take that for the agent
    /// not being installed.
    makes_dir: bool,
    /// What the hook must print, whatever happens: Antigravity wants a
    /// JSON object.
    answer: Option<&'static str>,
}

/// One hook crystal adds: the agent's event it runs on.
#[derive(Debug)]
struct Hook {
    event: &'static str,
    /// What it matches, in place of what the shape matches: Kimi's tools.
    matcher: Option<&'static str>,
    /// The event `crystal hook` takes it for, given with `--event`, when
    /// the agent's input may not name it, or names one crystal reads as
    /// another.
    taken_as: Option<&'static str>,
}

/// A hook on `event`, which the agent's input names.
const fn on(event: &'static str) -> Hook {
    Hook {
        event,
        matcher: None,
        taken_as: None,
    }
}

/// A hook on `event` that names it itself.
const fn naming(event: &'static str) -> Hook {
    Hook {
        event,
        matcher: None,
        taken_as: Some(event),
    }
}

/// How an agent's settings list its hooks.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// Claude Code's way: each event a list of groups, each group a
    /// matcher and its hooks, `{"matcher": "*", "hooks": [{"type":
    /// "command", "command": "…", "timeout": 10}]}`.
    Groups {
        matcher: Option<&'static str>,
        /// In the agent's own unit: seconds, but milliseconds for Qwen
        /// and Letta.
        timeout: u64,
        /// Letta's `quiet`, which keeps the hook's run out of its screen.
        quiet: bool,
    },
    /// GitHub Copilot's: each event a list of hooks, the command under
    /// `bash`, `{"type": "command", "bash": "…", "timeoutSec": 10}`.
    Flat,
    /// Cursor's: each event a list of `{"command": "…"}`, in a file that
    /// says its `version`.
    Simple,
    /// MastraCode's and Antigravity's: each event a list of `{"type":
    /// "command", "command": "…", "timeout": 10}`, with no groups.
    Plain {
        /// Milliseconds for MastraCode, seconds for Antigravity.
        timeout: u64,
    },
    /// Kimi's: `[[hooks]]` tables in its TOML config, each with its
    /// `event`, maybe a `matcher`, its `command` and a `timeout`.
    Tables,
}

/// Where in an agent's settings file the hooks are, by event.
#[derive(Debug, Clone, Copy)]
enum Place {
    /// Under `hooks`.
    Hooks,
    /// The file's top level is its hooks: MastraCode's.
    TopLevel,
    /// A block of crystal's own at the top level, under this name:
    /// Antigravity keys its hooks by whose they are.
    Block(&'static str),
}

pub const TARGETS: &[Target] = &[
    Target {
        agent: "cursor",
        env: &[("CURSOR_CONFIG_DIR", "")],
        home: ".cursor",
        file: "hooks.json",
        shape: Shape::Simple,
        place: Place::Hooks,
        hooks: &[on("sessionStart"), on("stop")],
        own_file: false,
        makes_dir: false,
        answer: None,
    },
    Target {
        agent: "droid",
        env: &[],
        home: ".factory",
        file: "settings.json",
        shape: Shape::Groups {
            matcher: None,
            timeout: 10,
            quiet: false,
        },
        place: Place::Hooks,
        hooks: &[on("SessionStart"), on("UserPromptSubmit"), on("Stop")],
        own_file: false,
        makes_dir: false,
        answer: None,
    },
    Target {
        agent: "qodercli",
        env: &[("QODER_CONFIG_DIR", "")],
        home: ".qoder",
        file: "settings.json",
        shape: Shape::Groups {
            matcher: Some("*"),
            timeout: 10,
            quiet: false,
        },
        place: Place::Hooks,
        hooks: &[
            on("SessionStart"),
            on("UserPromptSubmit"),
            on("PermissionRequest"),
            on("Stop"),
        ],
        own_file: false,
        makes_dir: false,
        answer: None,
    },
    Target {
        agent: "qwen",
        env: &[("QWEN_HOME", "")],
        home: ".qwen",
        file: "settings.json",
        shape: Shape::Groups {
            matcher: Some("*"),
            timeout: 10_000,
            quiet: false,
        },
        place: Place::Hooks,
        hooks: &[on("SessionStart")],
        own_file: false,
        makes_dir: false,
        answer: None,
    },
    Target {
        agent: "copilot",
        env: &[("COPILOT_HOME", "")],
        home: ".copilot",
        file: "settings.json",
        shape: Shape::Flat,
        place: Place::Hooks,
        hooks: &[on("SessionStart")],
        own_file: false,
        makes_dir: false,
        answer: None,
    },
    // Devin's hooks miss a permission it cancels and a turn cut short, so
    // they're trusted for when a turn starts and ends, and the screen says
    // the rest. Every one names its session, which it can change.
    Target {
        agent: "devin",
        env: &[("XDG_CONFIG_HOME", "devin")],
        home: ".config/devin",
        file: "config.json",
        shape: Shape::Groups {
            matcher: None,
            timeout: 10,
            quiet: false,
        },
        place: Place::Hooks,
        hooks: &[
            naming("SessionStart"),
            naming("UserPromptSubmit"),
            naming("Stop"),
        ],
        own_file: false,
        makes_dir: false,
        answer: None,
    },
    // Kimi's hooks cover its whole turn: its question to the user is a
    // tool, `AskUserQuestion`, which waits on them.
    Target {
        agent: "kimi",
        env: &[("KIMI_CODE_HOME", "")],
        home: ".kimi-code",
        file: "config.toml",
        shape: Shape::Tables,
        place: Place::Hooks,
        hooks: &[
            naming("SessionStart"),
            naming("UserPromptSubmit"),
            Hook {
                event: "PreToolUse",
                matcher: Some("^AskUserQuestion$"),
                taken_as: Some("PermissionRequest"),
            },
            Hook {
                event: "PostToolUse",
                matcher: Some("^AskUserQuestion$"),
                taken_as: Some("PostToolUse"),
            },
            naming("PermissionRequest"),
            naming("PermissionResult"),
            naming("Stop"),
            naming("Interrupt"),
        ],
        own_file: false,
        makes_dir: false,
        answer: None,
    },
    // What Letta's `SessionStart` hooks print goes into its next message:
    // crystal's print nothing.
    Target {
        agent: "letta",
        env: &[],
        home: ".letta",
        file: "settings.json",
        shape: Shape::Groups {
            matcher: None,
            timeout: 10_000,
            quiet: true,
        },
        place: Place::Hooks,
        hooks: &[naming("SessionStart")],
        own_file: false,
        makes_dir: false,
        answer: None,
    },
    Target {
        agent: "mastracode",
        env: &[],
        home: ".mastracode",
        file: "hooks.json",
        shape: Shape::Plain { timeout: 10_000 },
        place: Place::TopLevel,
        hooks: &[
            naming("SessionStart"),
            naming("UserPromptSubmit"),
            naming("PermissionRequest"),
            naming("PermissionResult"),
            naming("Interrupt"),
            naming("AgentEnd"),
            naming("Stop"),
        ],
        own_file: false,
        makes_dir: true,
        answer: None,
    },
    Target {
        agent: "grok",
        env: &[("GROK_CONFIG_DIR", ""), ("GROK_HOME", "")],
        home: ".grok",
        file: "hooks/crystal.json",
        shape: Shape::Groups {
            matcher: None,
            timeout: 10,
            quiet: false,
        },
        place: Place::Hooks,
        hooks: &[naming("SessionStart")],
        own_file: true,
        makes_dir: false,
        answer: None,
    },
    // Antigravity runs `PreInvocation` as a prompt is sent, which is the
    // first its conversation is named; its turns are read off its screen.
    Target {
        agent: "agy",
        env: &[("ANTIGRAVITY_CLI_CONFIG_DIR", "")],
        home: ".gemini/config",
        file: "hooks.json",
        shape: Shape::Plain { timeout: 10 },
        place: Place::Block("crystal"),
        hooks: &[naming("PreInvocation")],
        own_file: false,
        makes_dir: false,
        answer: Some("{}"),
    },
];

/// The agent `name`, an id or another name of one, if crystal can add
/// hooks to it.
pub fn target(name: &str) -> Option<&'static Target> {
    let registry = agent_rules::bundled_registry();
    let id = registry.find(name).map_or(name, |rules| rules.id.as_str());
    TARGETS.iter().find(|target| target.agent == id)
}

impl Target {
    /// Its settings directory, as this process's environment has it.
    pub fn dir(&self) -> PathBuf {
        let moved = self.env.iter().find_map(|(name, under)| {
            let dir = std::env::var_os(name).filter(|dir| !dir.is_empty())?;
            Some(PathBuf::from(dir).join(under))
        });
        match moved {
            Some(dir) => dir,
            None => {
                let home = std::env::var_os("HOME").unwrap_or_default();
                PathBuf::from(home).join(self.home)
            }
        }
    }

    /// The file its hooks go in, in `dir`.
    pub fn file(&self, dir: &Path) -> PathBuf {
        dir.join(self.file)
    }

    /// Whether crystal's hooks are in the settings in `dir`.
    pub fn installed(&self, dir: &Path) -> bool {
        let path = self.file(dir);
        if let Shape::Tables = self.shape {
            let Ok(text) = std::fs::read_to_string(&path) else {
                return false;
            };
            let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
                return false;
            };
            return tables(&document)
                .is_some_and(|tables| tables.iter().any(|table| table_is_ours(table, self.agent)));
        }
        let Ok(mut settings) = read_json(&path) else {
            return false;
        };
        let Some(hooks) = self.hooks_in(&mut settings) else {
            return false;
        };
        hooks
            .values()
            .filter_map(Value::as_array)
            .flatten()
            .any(|entry| holds_ours(entry, self.agent))
    }

    /// Puts crystal's hooks in the settings in `dir`, in place of any it
    /// put there before, for `crystal` to run. The files it changed.
    pub fn install(&self, dir: &Path, crystal: &Path) -> Result<Vec<PathBuf>> {
        if self.makes_dir && !dir.exists() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("couldn't make {}", shell::home_relative(dir)))?;
        }
        ensure!(
            dir.is_dir(),
            "{} isn't there: is {} installed?{}",
            shell::home_relative(dir),
            self.agent,
            self.env
                .first()
                .map(|(env, _)| format!(" (${env} says where it is)"))
                .unwrap_or_default()
        );
        let path = self.file(dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("couldn't make {}", shell::home_relative(parent)))?;
        }
        if let Shape::Tables = self.shape {
            return self.install_tables(&path, crystal);
        }
        let mut settings = if path.exists() {
            read_json(&path)?
        } else {
            json!({})
        };
        let Some(object) = settings.as_object_mut() else {
            bail!("{} isn't a JSON object", shell::home_relative(&path));
        };
        if matches!(self.shape, Shape::Simple) && !object.contains_key("version") {
            object.insert("version".to_string(), json!(1));
        }
        let hooks = self.hooks_of(object, &path)?;
        remove_ours(hooks, self.agent);
        for hook in self.hooks {
            let entries = hooks
                .entry(hook.event.to_string())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .with_context(|| format!("the hooks for {} aren't a list", hook.event))?;
            entries.push(self.entry(hook, &self.command(crystal, hook)));
        }
        let mut changed = Vec::new();
        if write_json(&path, &settings)? {
            changed.push(path);
        }
        Ok(changed)
    }

    /// Takes crystal's hooks out of the settings in `dir`, the user's own
    /// left as they are. The files it changed: none when there were none.
    pub fn uninstall(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let path = self.file(dir);
        if !path.exists() {
            return Ok(Vec::new());
        }
        if let Shape::Tables = self.shape {
            return self.uninstall_tables(&path);
        }
        let mut settings = read_json(&path)?;
        if !settings.is_object() {
            bail!("{} isn't a JSON object", shell::home_relative(&path));
        }
        let Some(hooks) = self.hooks_in(&mut settings) else {
            return Ok(Vec::new());
        };
        if !remove_ours(hooks, self.agent) {
            return Ok(Vec::new());
        }
        let emptied = hooks.is_empty();
        if self.own_file {
            std::fs::remove_file(&path)
                .with_context(|| format!("couldn't remove {}", shell::home_relative(&path)))?;
            return Ok(vec![path]);
        }
        if let (Place::Block(name), true) = (self.place, emptied) {
            settings
                .as_object_mut()
                .map(|object| object.shift_remove(name));
        }
        write_json(&path, &settings)?;
        Ok(vec![path])
    }

    /// The command one of its hooks runs: `crystal hook <agent>`, inside a
    /// crystal session only, and never failing.
    fn command(&self, crystal: &Path, hook: &Hook) -> String {
        let mut command = command(crystal, self.agent);
        if let Some(event) = hook.taken_as {
            let end = command.len() - " || true".len();
            command.insert_str(end, &format!(" --event {event}"));
        }
        if let Some(answer) = self.answer {
            command.push_str(&format!("; echo '{answer}'"));
        }
        command
    }

    /// One hook, in the agent's own shape.
    fn entry(&self, hook: &Hook, command: &str) -> Value {
        match self.shape {
            Shape::Groups {
                matcher,
                timeout,
                quiet,
            } => {
                let mut group = Map::new();
                if let Some(matcher) = hook.matcher.or(matcher) {
                    group.insert("matcher".to_string(), json!(matcher));
                }
                let mut inner = json!({"type": "command", "command": command, "timeout": timeout});
                if quiet {
                    inner["quiet"] = json!(true);
                }
                group.insert("hooks".to_string(), json!([inner]));
                Value::Object(group)
            }
            Shape::Flat => json!({"type": "command", "bash": command, "timeoutSec": 10}),
            Shape::Simple => json!({"command": command}),
            Shape::Plain { timeout } => {
                json!({"type": "command", "command": command, "timeout": timeout})
            }
            Shape::Tables => unreachable!("Kimi's hooks are tables"),
        }
    }

    /// Where the hooks are in `settings`, if they're there.
    fn hooks_in<'a>(&self, settings: &'a mut Value) -> Option<&'a mut Map<String, Value>> {
        let place = match self.place {
            Place::Hooks => settings.get_mut("hooks")?,
            Place::TopLevel => settings,
            Place::Block(name) => settings.get_mut(name)?,
        };
        place.as_object_mut()
    }

    /// Where the hooks go in `settings`, a settings file's top level, made
    /// when it isn't there.
    fn hooks_of<'a>(
        &self,
        settings: &'a mut Map<String, Value>,
        path: &Path,
    ) -> Result<&'a mut Map<String, Value>> {
        let shown = || shell::home_relative(path);
        let under = match self.place {
            Place::TopLevel => return Ok(settings),
            Place::Hooks => "hooks",
            Place::Block(name) => name,
        };
        settings
            .entry(under)
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .with_context(|| format!("the {under} in {} aren't a JSON object", shown()))
    }

    /// Puts crystal's hooks in Kimi's config at `path`, as tables after
    /// the user's.
    fn install_tables(&self, path: &Path, crystal: &Path) -> Result<Vec<PathBuf>> {
        let mut document = read_toml(path)?;
        let shown = shell::home_relative(path);
        let tables = document
            .entry("hooks")
            .or_insert_with(|| toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .with_context(|| format!("the hooks in {shown} aren't a list of tables"))?;
        tables.retain(|table| !table_is_ours(table, self.agent));
        for hook in self.hooks {
            let mut table = toml_edit::Table::new();
            table.insert("event", toml_edit::value(hook.event));
            if let Some(matcher) = hook.matcher {
                table.insert("matcher", toml_edit::value(matcher));
            }
            table.insert("command", toml_edit::value(self.command(crystal, hook)));
            table.insert("timeout", toml_edit::value(10));
            tables.push(table);
        }
        let mut changed = Vec::new();
        if write_whole(path, &document.to_string())? {
            changed.push(path.to_path_buf());
        }
        Ok(changed)
    }

    /// Takes crystal's tables out of Kimi's config at `path`.
    fn uninstall_tables(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let mut document = read_toml(path)?;
        let Some(tables) = document
            .get_mut("hooks")
            .and_then(toml_edit::Item::as_array_of_tables_mut)
        else {
            return Ok(Vec::new());
        };
        let had = tables.len();
        tables.retain(|table| !table_is_ours(table, self.agent));
        if tables.len() == had {
            return Ok(Vec::new());
        }
        if tables.is_empty() {
            document.remove("hooks");
        }
        write_whole(path, &document.to_string())?;
        Ok(vec![path.to_path_buf()])
    }
}

/// The command crystal's hook runs: `crystal hook <agent>`, inside a
/// crystal session only, and never failing.
pub fn command(crystal: &Path, agent: &str) -> String {
    let crystal = shell::quote(&crystal.to_string_lossy());
    format!("{GUARD}{crystal} hook {agent} || true")
}

/// What crystal's hook command starts with: outside a session, it's done.
const GUARD: &str = "[ -z \"${CRYSTAL_SESSION:-}\" ] || ";

/// Whether `command` is crystal's hook for `agent`, wherever crystal was
/// when it was put there, on whichever event, and whatever it prints.
fn is_ours(command: &str, agent: &str) -> bool {
    let Some(rest) = command.strip_prefix(GUARD) else {
        return false;
    };
    let Some((run, after)) = rest.rsplit_once(" || true") else {
        return false;
    };
    let answer = after
        .strip_prefix("; echo '")
        .and_then(|answer| answer.strip_suffix('\''));
    if !(after.is_empty() || answer.is_some_and(|answer| !answer.contains('\''))) {
        return false;
    }
    let run = match run.rsplit_once(" --event ") {
        Some((before, event)) if !event.is_empty() && !event.contains(' ') => before,
        _ => run,
    };
    run.ends_with(&format!(" hook {agent}"))
}

/// Whether a hook entry, of any shape, is or holds crystal's hook.
fn holds_ours(entry: &Value, agent: &str) -> bool {
    let own = ["command", "bash"]
        .iter()
        .filter_map(|field| entry[field].as_str())
        .any(|command| is_ours(command, agent));
    let inner = entry["hooks"]
        .as_array()
        .is_some_and(|hooks| hooks.iter().any(|hook| holds_ours(hook, agent)));
    own || inner
}

/// Takes crystal's hooks for `agent` out of every event's list: a group
/// that held only crystal's goes, and so does an event with nothing left.
/// Whether there were any.
fn remove_ours(hooks: &mut Map<String, Value>, agent: &str) -> bool {
    let mut removed = false;
    for entries in hooks.values_mut().filter_map(Value::as_array_mut) {
        entries.retain_mut(|entry| {
            if !holds_ours(entry, agent) {
                return true;
            }
            removed = true;
            match entry.get_mut("hooks").and_then(Value::as_array_mut) {
                Some(inner) => {
                    inner.retain(|hook| !holds_ours(hook, agent));
                    !inner.is_empty()
                }
                None => false,
            }
        });
    }
    hooks.retain(|_, entries| entries.as_array().is_none_or(|list| !list.is_empty()));
    removed
}

/// Kimi's `[[hooks]]` tables in its config, if it has them.
fn tables(document: &toml_edit::DocumentMut) -> Option<&toml_edit::ArrayOfTables> {
    document.get("hooks")?.as_array_of_tables()
}

/// Whether one of Kimi's hook tables is crystal's.
fn table_is_ours(table: &toml_edit::Table, agent: &str) -> bool {
    table
        .get("command")
        .and_then(toml_edit::Item::as_str)
        .is_some_and(|command| is_ours(command, agent))
}

fn read_json(path: &Path) -> Result<Value> {
    let shown = shell::home_relative(path);
    let text = std::fs::read_to_string(path).with_context(|| format!("couldn't read {shown}"))?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).with_context(|| format!("{shown} isn't JSON crystal can read"))
}

/// The TOML document at `path`, or an empty one when there's no file yet.
fn read_toml(path: &Path) -> Result<toml_edit::DocumentMut> {
    let shown = shell::home_relative(path);
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("couldn't read {shown}")),
    };
    text.parse()
        .with_context(|| format!("{shown} isn't TOML crystal can read"))
}

/// Writes `value` to `path` as formatted JSON, through a file beside it,
/// so that the settings are never left half written. Whether that changed
/// what was there.
fn write_json(path: &Path, value: &Value) -> Result<bool> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    write_whole(path, &text)
}

pub(crate) fn write_whole(path: &Path, text: &str) -> Result<bool> {
    if std::fs::read_to_string(path).is_ok_and(|was| was == text) {
        return Ok(false);
    }
    let shown = shell::home_relative(path);
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".crystal-new");
    let temporary = PathBuf::from(temporary);
    std::fs::write(&temporary, text).with_context(|| format!("couldn't write {shown}"))?;
    std::fs::rename(&temporary, path).with_context(|| format!("couldn't write {shown}"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn json_at(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    const CRYSTAL: &str = "/opt/my tools/crystal";

    #[test]
    fn the_hook_runs_crystal_only_in_a_session_and_never_fails() {
        let command = command(Path::new(CRYSTAL), "droid");
        assert_eq!(
            command,
            "[ -z \"${CRYSTAL_SESSION:-}\" ] || '/opt/my tools/crystal' hook droid || true"
        );
        assert!(is_ours(&command, "droid"));
        assert!(!is_ours(&command, "qwen"));
        assert!(!is_ours("crystal hook droid", "droid"));
        let run = |session: Option<&str>| {
            let mut sh = std::process::Command::new("sh");
            sh.arg("-c")
                .arg(command.replace("'/opt/my tools/crystal'", "false"));
            sh.env_remove("CRYSTAL_SESSION");
            if let Some(session) = session {
                sh.env("CRYSTAL_SESSION", session);
            }
            sh.status().unwrap().success()
        };
        assert!(run(None));
        assert!(
            run(Some("fix-it")),
            "a hook that fails mustn't fail the agent"
        );
    }

    #[test]
    fn the_users_own_hooks_stay_through_install_and_uninstall() {
        let dir = dir();
        let settings = dir.path().join("settings.json");
        let own = json!({
            "model": "x",
            "hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": "say done"}]}],
            }
        });
        std::fs::write(&settings, own.to_string()).unwrap();
        let droid = target("droid").unwrap();
        droid.install(dir.path(), Path::new(CRYSTAL)).unwrap();
        let installed = json_at(&settings);
        assert_eq!(installed["model"], "x");
        let stop = installed["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "say done");
        // Installed again from a crystal somewhere else, it's replaced.
        droid
            .install(dir.path(), Path::new("/usr/bin/crystal"))
            .unwrap();
        let again = json_at(&settings);
        assert_eq!(again["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert!(again.to_string().contains("/usr/bin/crystal hook droid"));

        assert_eq!(
            droid.uninstall(dir.path()).unwrap(),
            std::slice::from_ref(&settings)
        );
        assert_eq!(json_at(&settings), own);
        assert!(!droid.installed(dir.path()));
        assert!(droid.uninstall(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_group_holding_the_users_hook_beside_crystal_s_keeps_the_users() {
        let mut hooks = json!({
            "Stop": [{"hooks": [
                {"type": "command", "command": "say done"},
                {"type": "command", "command": command(Path::new(CRYSTAL), "qodercli")},
            ]}],
            "SessionStart": [{"hooks": [{"type": "command", "command": command(Path::new(CRYSTAL), "qodercli")}]}],
        });
        assert!(remove_ours(hooks.as_object_mut().unwrap(), "qodercli"));
        assert_eq!(
            hooks,
            json!({"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]})
        );
    }

    #[test]
    fn each_agent_gets_hooks_in_its_own_shape() {
        let dir = dir();
        let cursor = target("cursor-agent").unwrap();
        cursor.install(dir.path(), Path::new(CRYSTAL)).unwrap();
        let file = json_at(&dir.path().join("hooks.json"));
        assert_eq!(file["version"], 1);
        assert!(is_ours(
            file["hooks"]["stop"][0]["command"].as_str().unwrap(),
            "cursor"
        ));

        let dir = self::dir();
        let copilot = target("copilot").unwrap();
        copilot.install(dir.path(), Path::new(CRYSTAL)).unwrap();
        let file = json_at(&dir.path().join("settings.json"));
        let hook = &file["hooks"]["SessionStart"][0];
        assert!(is_ours(hook["bash"].as_str().unwrap(), "copilot"));
        assert_eq!(hook["timeoutSec"], 10);

        let dir = self::dir();
        let qwen = target("qwen").unwrap();
        qwen.install(dir.path(), Path::new(CRYSTAL)).unwrap();
        let file = json_at(&dir.path().join("settings.json"));
        let group = &file["hooks"]["SessionStart"][0];
        assert_eq!(group["matcher"], "*");
        assert_eq!(group["hooks"][0]["timeout"], 10_000);
    }

    #[test]
    fn an_agent_that_isnt_there_or_a_file_that_isnt_json_is_left_alone() {
        let dir = dir();
        let droid = target("droid").unwrap();
        let gone = dir.path().join("nowhere");
        let err = droid.install(&gone, Path::new(CRYSTAL)).unwrap_err();
        assert!(err.to_string().contains("is droid installed?"), "{err}");

        std::fs::write(dir.path().join("settings.json"), "{ not json").unwrap();
        assert!(droid.install(dir.path(), Path::new(CRYSTAL)).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn a_hook_that_names_its_event_or_prints_an_answer_is_still_crystal_s() {
        let agy = target("antigravity").unwrap();
        let hook = &agy.hooks[0];
        let command = agy.command(Path::new(CRYSTAL), hook);
        assert_eq!(
            command,
            "[ -z \"${CRYSTAL_SESSION:-}\" ] || '/opt/my tools/crystal' hook agy --event PreInvocation || true; echo '{}'"
        );
        assert!(is_ours(&command, "agy"));
        assert!(!is_ours(&command, "kimi"));
        assert!(!is_ours(&format!("{command}; rm -rf x"), "agy"));
        // It answers whatever happens, in a session or out of one.
        for session in [None, Some("fix-it")] {
            let mut sh = std::process::Command::new("sh");
            sh.arg("-c")
                .arg(command.replace("'/opt/my tools/crystal'", "false"));
            sh.env_remove("CRYSTAL_SESSION");
            if let Some(session) = session {
                sh.env("CRYSTAL_SESSION", session);
            }
            let out = sh.output().unwrap();
            assert!(out.status.success());
            assert_eq!(out.stdout, b"{}\n");
        }
    }

    #[test]
    fn kimi_s_hooks_are_tables_after_the_user_s_in_its_config() {
        let dir = dir();
        let config = dir.path().join("config.toml");
        let own = "# mine\nmodel = \"k2\"\n\n[[hooks]]\nevent = \"Stop\"\ncommand = \"say done\"\n";
        std::fs::write(&config, own).unwrap();
        let kimi = target("kimi-code").unwrap();
        kimi.install(dir.path(), Path::new(CRYSTAL)).unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.starts_with(own), "{text}");
        let document: toml_edit::DocumentMut = text.parse().unwrap();
        let listed = tables(&document).unwrap();
        assert_eq!(listed.len(), 1 + kimi.hooks.len());
        let asks = listed
            .iter()
            .find(|table| {
                table.get("matcher").is_some() && table["event"].as_str() == Some("PreToolUse")
            })
            .unwrap();
        assert_eq!(asks["matcher"].as_str(), Some("^AskUserQuestion$"));
        let command = asks["command"].as_str().unwrap();
        assert!(
            command.ends_with(" hook kimi --event PermissionRequest || true"),
            "{command}"
        );
        assert!(kimi.installed(dir.path()));
        // Again, from elsewhere: replaced, not added to.
        kimi.install(dir.path(), Path::new("/usr/bin/crystal"))
            .unwrap();
        let again: toml_edit::DocumentMut =
            std::fs::read_to_string(&config).unwrap().parse().unwrap();
        assert_eq!(tables(&again).unwrap().len(), 1 + kimi.hooks.len());

        assert_eq!(
            kimi.uninstall(dir.path()).unwrap(),
            std::slice::from_ref(&config)
        );
        assert_eq!(std::fs::read_to_string(&config).unwrap(), own);
        assert!(!kimi.installed(dir.path()));
        assert!(kimi.uninstall(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn mastracode_s_hooks_are_its_file_s_top_level() {
        let dir = dir();
        let mastracode = target("mastracode").unwrap();
        let home = dir.path().join(".mastracode");
        // MastraCode needn't have made its directory.
        mastracode.install(&home, Path::new(CRYSTAL)).unwrap();
        let file = json_at(&home.join("hooks.json"));
        assert!(file.get("hooks").is_none());
        let hook = &file["AgentEnd"][0];
        assert_eq!(hook["type"], "command");
        assert_eq!(hook["timeout"], 10_000);
        assert!(
            hook["command"]
                .as_str()
                .unwrap()
                .ends_with(" hook mastracode --event AgentEnd || true")
        );
        assert!(mastracode.installed(&home));
        mastracode.uninstall(&home).unwrap();
        assert_eq!(json_at(&home.join("hooks.json")), json!({}));
    }

    #[test]
    fn antigravity_s_hooks_are_a_block_of_crystal_s_own() {
        let dir = dir();
        let file = dir.path().join("hooks.json");
        let own = json!({"mine": {"PreInvocation": [{"type": "command", "command": "say hi"}]}});
        std::fs::write(&file, own.to_string()).unwrap();
        let agy = target("agy").unwrap();
        agy.install(dir.path(), Path::new(CRYSTAL)).unwrap();
        let installed = json_at(&file);
        assert_eq!(installed["mine"], own["mine"]);
        let hook = &installed["crystal"]["PreInvocation"][0];
        assert_eq!(hook["timeout"], 10);
        assert!(hook.get("hooks").is_none(), "no groups for PreInvocation");
        assert!(agy.installed(dir.path()));
        agy.uninstall(dir.path()).unwrap();
        assert_eq!(json_at(&file), own);
    }

    #[test]
    fn grok_s_hooks_are_a_file_of_crystal_s_own() {
        let dir = dir();
        let grok = target("grok").unwrap();
        grok.install(dir.path(), Path::new(CRYSTAL)).unwrap();
        let file = dir.path().join("hooks/crystal.json");
        let hook = &json_at(&file)["hooks"]["SessionStart"][0]["hooks"][0];
        assert_eq!(hook["timeout"], 10);
        assert!(grok.installed(dir.path()));
        assert_eq!(
            grok.uninstall(dir.path()).unwrap(),
            std::slice::from_ref(&file)
        );
        assert!(!file.exists());
        assert!(dir.path().join("hooks").is_dir());
    }

    #[test]
    fn letta_s_hook_is_quiet_and_waits_in_milliseconds() {
        let dir = dir();
        let letta = target("letta-code").unwrap();
        letta.install(dir.path(), Path::new(CRYSTAL)).unwrap();
        let file = json_at(&dir.path().join("settings.json"));
        let hook = &file["hooks"]["SessionStart"][0]["hooks"][0];
        assert_eq!(hook["quiet"], true);
        assert_eq!(hook["timeout"], 10_000);
        assert!(
            hook["command"]
                .as_str()
                .unwrap()
                .ends_with(" hook letta --event SessionStart || true")
        );
    }

    #[test]
    fn only_agents_with_hooks_in_their_settings_are_targets() {
        assert!(
            target("claude").is_none(),
            "Claude's go on its command line"
        );
        assert!(target("gemini").is_none());
        assert!(
            target("codex").is_none(),
            "Codex's are crystal integration's"
        );
        assert_eq!(target("kiro-cli").map(|t| t.agent), None);
        assert_eq!(target("cursor-agent").map(|t| t.agent), Some("cursor"));
        assert!(target("pi").is_none(), "Pi takes a plugin");
        // Each is an agent crystal has rules for, by its id.
        for target in TARGETS {
            assert!(agent_rules::bundled_registry().find(target.agent).is_some());
        }
    }
}
