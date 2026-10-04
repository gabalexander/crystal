//! crystal's hooks in the own settings of agents that take hooks from
//! nowhere else, beyond Claude Code and Codex, which [`crate::integration`]
//! handles itself: Cursor, Droid, Qoder, Qwen Code and GitHub Copilot. Put
//! there when the user asks, with `crystal integration install <agent>`,
//! and taken out with `uninstall`.
//!
//! The hook runs `crystal hook <agent>` only inside a crystal session, so
//! the agent run anywhere else goes on as before, and a hook that fails
//! never holds it up. crystal finds its own hooks again by their command,
//! so taking them out leaves the user's alone. The settings file is written
//! again whole, as formatted JSON, its keys in order.
//!
//! The agents and their files are as herdr installs its hooks for them.
//! Each gets the events that say what it's doing and which conversation
//! it's in, as far as it has them.

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
    /// The variable that moves its settings directory, if it has one.
    env: Option<&'static str>,
    /// Its settings directory, in the home directory.
    home: &'static str,
    /// The file in that directory its hooks go in.
    file: &'static str,
    shape: Shape,
    /// The events the hook runs on.
    events: &'static [&'static str],
}

/// How an agent's settings list its hooks.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// Claude Code's way: each event a list of groups, each group a
    /// matcher and its hooks, `{"matcher": "*", "hooks": [{"type":
    /// "command", "command": "…", "timeout": 10}]}`.
    Groups {
        matcher: Option<&'static str>,
        /// In the agent's own unit: seconds, but milliseconds for Qwen.
        timeout: u64,
    },
    /// GitHub Copilot's: each event a list of hooks, the command under
    /// `bash`, `{"type": "command", "bash": "…", "timeoutSec": 10}`.
    Flat,
    /// Cursor's: each event a list of `{"command": "…"}`, in a file that
    /// says its `version`.
    Simple,
}

pub const TARGETS: &[Target] = &[
    Target {
        agent: "cursor",
        env: Some("CURSOR_CONFIG_DIR"),
        home: ".cursor",
        file: "hooks.json",
        shape: Shape::Simple,
        events: &["sessionStart", "stop"],
    },
    Target {
        agent: "droid",
        env: None,
        home: ".factory",
        file: "settings.json",
        shape: Shape::Groups {
            matcher: None,
            timeout: 10,
        },
        events: &["SessionStart", "UserPromptSubmit", "Stop"],
    },
    Target {
        agent: "qodercli",
        env: Some("QODER_CONFIG_DIR"),
        home: ".qoder",
        file: "settings.json",
        shape: Shape::Groups {
            matcher: Some("*"),
            timeout: 10,
        },
        events: &[
            "SessionStart",
            "UserPromptSubmit",
            "PermissionRequest",
            "Stop",
        ],
    },
    Target {
        agent: "qwen",
        env: Some("QWEN_HOME"),
        home: ".qwen",
        file: "settings.json",
        shape: Shape::Groups {
            matcher: Some("*"),
            timeout: 10_000,
        },
        events: &["SessionStart"],
    },
    Target {
        agent: "copilot",
        env: Some("COPILOT_HOME"),
        home: ".copilot",
        file: "settings.json",
        shape: Shape::Flat,
        events: &["SessionStart"],
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
        let moved = self
            .env
            .and_then(std::env::var_os)
            .filter(|dir| !dir.is_empty());
        match moved {
            Some(dir) => PathBuf::from(dir),
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
        let Ok(settings) = read_json(&self.file(dir)) else {
            return false;
        };
        let Some(hooks) = settings.get("hooks").and_then(Value::as_object) else {
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
        ensure!(
            dir.is_dir(),
            "{} isn't there: is {} installed?{}",
            shell::home_relative(dir),
            self.agent,
            self.env
                .map(|env| format!(" (${env} says where it is)"))
                .unwrap_or_default()
        );
        let path = self.file(dir);
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
        let hooks = hooks_of(object, &path)?;
        remove_ours(hooks, self.agent);
        let command = command(crystal, self.agent);
        for event in self.events {
            let entries = hooks
                .entry(event.to_string())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .with_context(|| format!("the hooks for {event} aren't a list"))?;
            entries.push(self.entry(&command));
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
        let mut settings = read_json(&path)?;
        let Some(object) = settings.as_object_mut() else {
            bail!("{} isn't a JSON object", shell::home_relative(&path));
        };
        let Some(hooks) = object.get_mut("hooks").and_then(Value::as_object_mut) else {
            return Ok(Vec::new());
        };
        if !remove_ours(hooks, self.agent) {
            return Ok(Vec::new());
        }
        write_json(&path, &settings)?;
        Ok(vec![path])
    }

    /// One hook, in the agent's own shape.
    fn entry(&self, command: &str) -> Value {
        match self.shape {
            Shape::Groups { matcher, timeout } => {
                let mut group = Map::new();
                if let Some(matcher) = matcher {
                    group.insert("matcher".to_string(), json!(matcher));
                }
                let hook = json!({"type": "command", "command": command, "timeout": timeout});
                group.insert("hooks".to_string(), json!([hook]));
                Value::Object(group)
            }
            Shape::Flat => json!({"type": "command", "bash": command, "timeoutSec": 10}),
            Shape::Simple => json!({"command": command}),
        }
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
/// when it was put there.
fn is_ours(command: &str, agent: &str) -> bool {
    command.starts_with(GUARD) && command.ends_with(&format!(" hook {agent} || true"))
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

/// The `hooks` object of a settings file's top level, made when it has
/// none.
fn hooks_of<'a>(
    settings: &'a mut Map<String, Value>,
    path: &Path,
) -> Result<&'a mut Map<String, Value>> {
    settings
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .with_context(|| {
            format!(
                "the hooks in {} aren't a JSON object",
                shell::home_relative(path)
            )
        })
}

fn read_json(path: &Path) -> Result<Value> {
    let shown = shell::home_relative(path);
    let text = std::fs::read_to_string(path).with_context(|| format!("couldn't read {shown}"))?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).with_context(|| format!("{shown} isn't JSON crystal can read"))
}

/// Writes `value` to `path` as formatted JSON, through a file beside it,
/// so that the settings are never left half written. Whether that changed
/// what was there.
fn write_json(path: &Path, value: &Value) -> Result<bool> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    write_whole(path, &text)
}

fn write_whole(path: &Path, text: &str) -> Result<bool> {
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
    }
}
