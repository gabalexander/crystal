//! A plugin's `plugin.toml`: what it's called, and what it adds to crystal.
//!
//! - `[[actions]]`: commands the user runs, from the TUI's plugins view, a
//!   key of their own in the sidebar, or `crystal plugin run`;
//! - `[[events]]`: commands the daemon runs when something happens, like a
//!   session starting to wait on the user;
//! - `[[panes]]`: programs the TUI shows over its panes, with the keyboard,
//!   so a plugin can be a whole TUI of its own.
//!
//! A command is a list of words, run without a shell, from the plugin's
//! own directory: `["sh", "hook.sh"]`.

use anyhow::{Result, bail};
use serde::Deserialize;

/// What happens in crystal that a plugin can hear about.
pub const EVENTS: &[&str] = &[
    "session.started",
    "session.waiting",
    "session.done",
    "session.ended",
    "task.closed",
    "worktree.created",
    "worktree.removed",
];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// What the plugin is called, which is also its directory's name.
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub events: Vec<EventHook>,
    #[serde(default)]
    pub panes: Vec<PaneSpec>,
}

/// A command the user runs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub command: Vec<String>,
    /// A key in the TUI's sidebar that runs it: one character crystal
    /// doesn't use itself.
    #[serde(default)]
    pub key: Option<String>,
}

/// A command the daemon runs when something happens.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventHook {
    /// The event, or a pattern of them: `session.waiting`, `session.*`,
    /// or `*` for every one.
    pub on: String,
    pub command: Vec<String>,
}

/// A program shown over the TUI's panes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneSpec {
    pub id: String,
    pub title: String,
    pub command: Vec<String>,
}

impl Manifest {
    /// Reads a manifest, and checks that it makes sense for a plugin kept
    /// in a directory called `dir_name`.
    pub fn parse(text: &str, dir_name: &str) -> Result<Manifest> {
        let manifest: Manifest = toml::from_str(text)?;
        manifest.check(dir_name)?;
        Ok(manifest)
    }

    fn check(&self, dir_name: &str) -> Result<()> {
        check_name(&self.name)?;
        if self.name != dir_name {
            bail!(
                "the plugin is called {}, but its directory is {dir_name}: they have to match",
                self.name
            );
        }
        if self.version.trim().is_empty() {
            bail!("the plugin has no version");
        }
        let mut ids: Vec<&str> = Vec::new();
        for action in &self.actions {
            check_id(&action.id, &mut ids)?;
            check_command(&action.command, &format!("action {}", action.id))?;
            if let Some(key) = &action.key {
                check_key(key, &action.id)?;
            }
        }
        for pane in &self.panes {
            check_id(&pane.id, &mut ids)?;
            check_command(&pane.command, &format!("pane {}", pane.id))?;
        }
        for hook in &self.events {
            if !EVENTS.iter().any(|event| matches(&hook.on, event)) {
                bail!(
                    "events: `{}` matches no event; they are {}",
                    hook.on,
                    EVENTS.join(", ")
                );
            }
            check_command(&hook.command, &format!("the hook on {}", hook.on))?;
        }
        Ok(())
    }

    /// The commands the plugin can run, each with what runs it, for the
    /// user to read before installing it.
    pub fn commands(&self) -> Vec<(String, &[String])> {
        let mut commands: Vec<(String, &[String])> = Vec::new();
        for action in &self.actions {
            commands.push((format!("action {}", action.id), &action.command));
        }
        for hook in &self.events {
            commands.push((format!("on {}", hook.on), &hook.command));
        }
        for pane in &self.panes {
            commands.push((format!("pane {}", pane.id), &pane.command));
        }
        commands
    }
}

/// Whether the event called `event` is one `pattern` asks for: the same
/// name, a family like `session.*`, or `*` for every one.
pub fn matches(pattern: &str, event: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.strip_suffix(".*") {
        Some(family) => event
            .strip_prefix(family)
            .is_some_and(|rest| rest.starts_with('.')),
        None => pattern == event,
    }
}

/// A plugin's name: lowercase letters, digits and dashes, so it can be a
/// directory, a key in `[plugins]`, and part of a session's name.
pub fn check_name(name: &str) -> Result<()> {
    let fine = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !fine {
        bail!("`{name}` can't be a plugin's name: use lowercase letters, digits and dashes");
    }
    if crate::plugins::is_built_in(name) {
        bail!("`{name}` is the name of one of crystal's own plugins");
    }
    Ok(())
}

fn check_id<'a>(id: &'a str, seen: &mut Vec<&'a str>) -> Result<()> {
    if id.is_empty() || id.contains(char::is_whitespace) {
        bail!("`{id}` can't be an id: it has to be one word");
    }
    if seen.contains(&id) {
        bail!("two of the plugin's actions or panes are called {id}");
    }
    seen.push(id);
    Ok(())
}

fn check_command(command: &[String], what: &str) -> Result<()> {
    if command.first().is_none_or(|program| program.is_empty()) {
        bail!("{what} has no command");
    }
    Ok(())
}

/// An action's key: one character, and not one crystal's sidebar uses.
fn check_key(key: &str, action: &str) -> Result<()> {
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        bail!("action {action}: a key is one character, not `{key}`");
    };
    if crate::plugins::RESERVED_KEYS.contains(c) || c.is_whitespace() {
        bail!("action {action}: crystal uses `{key}` itself");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
name = "notes"
version = "0.1.0"
description = "Keeps notes"

[[actions]]
id = "note"
title = "Add a note"
command = ["sh", "note.sh"]
key = "N"

[[events]]
on = "session.*"
command = ["sh", "hook.sh"]

[[panes]]
id = "board"
title = "Board"
command = ["sh", "board.sh"]
"#;

    #[test]
    fn a_whole_manifest_is_read() {
        let manifest = Manifest::parse(GOOD, "notes").unwrap();
        assert_eq!(manifest.actions[0].key.as_deref(), Some("N"));
        assert_eq!(manifest.events[0].on, "session.*");
        assert_eq!(manifest.panes[0].title, "Board");
        assert_eq!(manifest.commands().len(), 3);
    }

    #[test]
    fn a_manifest_that_doesnt_make_sense_says_why() {
        let cases = [
            (
                GOOD.replace("name = \"notes\"", "name = \"Notes!\""),
                "can't be a plugin's name",
            ),
            (GOOD.to_string(), "directory is other"),
            (
                GOOD.replace("key = \"N\"", "key = \"j\""),
                "crystal uses `j`",
            ),
            (GOOD.replace("key = \"N\"", "key = \"NN\""), "one character"),
            (
                GOOD.replace("on = \"session.*\"", "on = \"sesion.*\""),
                "matches no event",
            ),
            (
                GOOD.replace("id = \"board\"", "id = \"note\""),
                "two of the plugin's",
            ),
            (
                GOOD.replace("[\"sh\", \"hook.sh\"]", "[]"),
                "has no command",
            ),
            (
                GOOD.replace("version = \"0.1.0\"", "colour = \"red\""),
                "colour",
            ),
        ];
        for (text, expected) in cases {
            let dir = if expected.contains("directory") {
                "other"
            } else {
                "notes"
            };
            let err = Manifest::parse(&text, dir).unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{expected}: {err:#}");
        }
    }

    #[test]
    fn a_plugin_cant_take_the_name_of_one_of_crystals() {
        let err = check_name("memory").unwrap_err();
        assert!(format!("{err:#}").contains("crystal's own"), "{err:#}");
    }

    #[test]
    fn patterns_match_events_by_name_family_or_all() {
        assert!(matches("session.waiting", "session.waiting"));
        assert!(!matches("session.waiting", "session.done"));
        assert!(matches("session.*", "session.done"));
        assert!(!matches("session.*", "sessions.done"));
        assert!(!matches("session.*", "task.closed"));
        assert!(matches("*", "worktree.created"));
    }
}
