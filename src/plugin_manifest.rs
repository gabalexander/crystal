//! A plugin's `plugin.toml`: what it's called, where it runs, and what it
//! adds to crystal.
//!
//! - `[[build]]`: commands run once as it's installed, and again with
//!   `crystal plugin build`;
//! - `[[startup]]`: commands the daemon runs once as it starts;
//! - `[[actions]]`: commands the user runs, from the TUI's plugins view, a
//!   key of their own in the sidebar, or `crystal plugin run`;
//! - `[[events]]`: commands the daemon runs when something happens, like a
//!   session starting to wait on the user;
//! - `[[panes]]`: programs the TUI shows over its panes, with the keyboard,
//!   so a plugin can be a whole TUI of its own;
//! - `[[link_handlers]]`: links a Ctrl+click in a pane hands to one of its
//!   actions rather than to the browser.
//!
//! A command is a list of words, run without a shell, from the plugin's
//! own directory: `["sh", "hook.sh"]`.

use crate::events;
use crate::tui::keymap::Sequence;
use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// What the plugin is called, which is also its directory's name.
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// The oldest crystal the plugin works with, like `0.3.0`.
    #[serde(default)]
    pub min_crystal_version: Option<String>,
    /// The systems the plugin runs on; any, when it doesn't say.
    #[serde(default)]
    pub platforms: Option<Vec<Platform>>,
    #[serde(default)]
    pub build: Vec<Once>,
    #[serde(default)]
    pub startup: Vec<Once>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub events: Vec<EventHook>,
    #[serde(default)]
    pub panes: Vec<PaneSpec>,
    #[serde(default)]
    pub link_handlers: Vec<LinkHandler>,
}

/// A system crystal runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Linux,
}

impl Platform {
    /// The system this crystal was built for.
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::Macos
        } else {
            Platform::Linux
        }
    }

    fn name(self) -> &'static str {
        match self {
            Platform::Macos => "macos",
            Platform::Linux => "linux",
        }
    }
}

/// A command run once at a point in the plugin's life: as it's built, or
/// as the daemon starts.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Once {
    pub command: Vec<String>,
    /// The systems it runs on; every one the plugin does, when it doesn't
    /// say.
    #[serde(default)]
    pub platforms: Option<Vec<Platform>>,
}

impl Once {
    /// Whether it runs on this system.
    pub fn runs_here(&self) -> bool {
        runs_on(self.platforms.as_deref(), Platform::current())
    }
}

/// A command the user runs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub command: Vec<String>,
    /// A key in the TUI's sidebar that runs it, one crystal doesn't use
    /// itself: a character, a chord like `ctrl+alt+n`, or two keys one
    /// after the other, like `N t`.
    #[serde(default)]
    pub key: Option<String>,
}

/// A command the daemon runs when something happens.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventHook {
    /// The event, or a pattern of them: `session.waiting`, `session.*`,
    /// or `*` for every one. See [`events::Kind`].
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

/// Links one of the plugin's actions opens, in place of the browser.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkHandler {
    /// A regular expression, found anywhere in a link unless `^` and `$`
    /// pin it to the link's start and end.
    pub pattern: String,
    /// The id of the action run on a link that matches.
    pub action: String,
}

impl LinkHandler {
    /// Whether it takes `url`.
    pub fn takes(&self, url: &str) -> bool {
        Regex::new(&self.pattern).is_ok_and(|pattern| pattern.is_match(url))
    }
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
        if let Some(min) = &self.min_crystal_version
            && version(min).is_none()
        {
            bail!("min_crystal_version `{min}` isn't a version like 0.3.0");
        }
        check_platforms(self.platforms.as_deref(), "platforms")?;
        for (what, commands) in [("build", &self.build), ("startup", &self.startup)] {
            for once in commands {
                check_command(&once.command, &format!("a {what} command"))?;
                let platforms = format!("a {what} command's platforms");
                check_platforms(once.platforms.as_deref(), &platforms)?;
            }
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
            events::check_pattern(&hook.on).context("events")?;
            check_command(&hook.command, &format!("the hook on {}", hook.on))?;
        }
        for handler in &self.link_handlers {
            if let Err(err) = Regex::new(&handler.pattern) {
                bail!("link handler `{}`: {err}", handler.pattern);
            }
            self.action(&handler.action).with_context(|| {
                format!(
                    "link handler `{}` runs action {}, which the plugin doesn't have",
                    handler.pattern, handler.action
                )
            })?;
        }
        Ok(())
    }

    /// The action called `id`.
    pub fn action(&self, id: &str) -> Option<&Action> {
        self.actions.iter().find(|action| action.id == id)
    }

    /// Why the plugin can't run in this crystal, on this system, if it
    /// can't.
    pub fn unfit(&self) -> Option<String> {
        self.unfit_for(env!("CARGO_PKG_VERSION"), Platform::current())
    }

    fn unfit_for(&self, crystal: &str, platform: Platform) -> Option<String> {
        if let Some(min) = &self.min_crystal_version
            && version(min) > version(crystal)
        {
            return Some(format!("needs crystal {min} or later; this is {crystal}"));
        }
        if !runs_on(self.platforms.as_deref(), platform) {
            let names: Vec<&str> = self.platforms.iter().flatten().map(|p| p.name()).collect();
            return Some(format!(
                "runs only on {}; this is {}",
                names.join(" and "),
                platform.name()
            ));
        }
        None
    }

    /// The commands the plugin can run, each with what runs it, for the
    /// user to read before installing it.
    pub fn commands(&self) -> Vec<(String, &[String])> {
        let mut commands: Vec<(String, &[String])> = Vec::new();
        for once in &self.build {
            commands.push(("build".to_string(), &once.command));
        }
        for once in &self.startup {
            commands.push(("at startup".to_string(), &once.command));
        }
        for action in &self.actions {
            commands.push((format!("action {}", action.id), &action.command));
        }
        for hook in &self.events {
            commands.push((format!("on {}", hook.on), &hook.command));
        }
        for pane in &self.panes {
            commands.push((format!("pane {}", pane.id), &pane.command));
        }
        for handler in &self.link_handlers {
            if let Some(action) = self.action(&handler.action) {
                commands.push((format!("links {}", handler.pattern), &action.command));
            }
        }
        commands
    }
}

/// Whether something that runs on `platforms`, or anywhere with `None`,
/// runs on `platform`.
fn runs_on(platforms: Option<&[Platform]>, platform: Platform) -> bool {
    platforms.is_none_or(|platforms| platforms.contains(&platform))
}

fn check_platforms(platforms: Option<&[Platform]>, what: &str) -> Result<()> {
    if platforms.is_some_and(|platforms| platforms.is_empty()) {
        bail!("{what} is empty: leave it out to run anywhere");
    }
    Ok(())
}

/// A version's numbers, `0.3` as `0.3.0`, to compare; `None` for what
/// isn't one.
fn version(text: &str) -> Option<[u64; 3]> {
    let mut numbers = [0; 3];
    let mut parts = text.trim().split('.');
    for number in &mut numbers {
        match parts.next() {
            Some(part) if !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()) => {
                *number = part.parse().ok()?;
            }
            Some(_) => return None,
            None => break,
        }
    }
    parts.next().is_none().then_some(numbers)
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

/// An action's key: one, or two pressed one after the other, and not one
/// crystal's sidebar uses.
fn check_key(key: &str, action: &str) -> Result<()> {
    let sequence = Sequence::parse(key).map_err(|why| anyhow::anyhow!("action {action}: {why}"))?;
    sequence
        .check()
        .map_err(|why| anyhow::anyhow!("action {action}: {why}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
name = "notes"
version = "0.1.0"
description = "Keeps notes"
min_crystal_version = "0.1"
platforms = ["macos", "linux"]

[[build]]
command = ["make"]

[[build]]
command = ["brew", "install", "jq"]
platforms = ["macos"]

[[startup]]
command = ["sh", "restore.sh"]

[[link_handlers]]
pattern = "^https://github\\.com/.+/issues/[0-9]+$"
action = "note"

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
        assert_eq!(manifest.commands().len(), 7);
        assert_eq!(manifest.build[1].platforms, Some(vec![Platform::Macos]));
        assert_eq!(manifest.startup[0].command, ["sh", "restore.sh"]);
        assert_eq!(manifest.unfit(), None);
    }

    #[test]
    fn a_link_handler_takes_the_links_its_pattern_matches() {
        let manifest = Manifest::parse(GOOD, "notes").unwrap();
        let handler = &manifest.link_handlers[0];
        assert!(handler.takes("https://github.com/a/b/issues/12"));
        assert!(!handler.takes("https://github.com/a/b/pull/12"));
        assert!(!handler.takes("https://github.com/a/b/issues/12#top"));
    }

    #[test]
    fn a_plugin_fits_a_crystal_new_enough_on_a_system_it_names() {
        let manifest = Manifest::parse(GOOD, "notes").unwrap();
        assert_eq!(manifest.unfit_for("0.1.0", Platform::Linux), None);
        assert_eq!(
            manifest.unfit_for("0.0.9", Platform::Linux).unwrap(),
            "needs crystal 0.1 or later; this is 0.0.9"
        );
        let linux = GOOD.replace(r#"["macos", "linux"]"#, r#"["linux"]"#);
        let manifest = Manifest::parse(&linux, "notes").unwrap();
        assert_eq!(
            manifest.unfit_for("0.3.0", Platform::Macos).unwrap(),
            "runs only on linux; this is macos"
        );
        // A newer crystal is fine, however the numbers compare as text.
        assert_eq!(manifest.unfit_for("0.10.0", Platform::Linux), None);
    }

    #[test]
    fn versions_compare_by_their_numbers() {
        assert_eq!(version("0.3"), Some([0, 3, 0]));
        assert!(version("0.10.0") > version("0.9.1"));
        for text in ["", "0.3.0-beta", "1.2.3.4", "v1", "1..2"] {
            assert_eq!(version(text), None, "{text}");
        }
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
            (
                GOOD.replace("key = \"N\"", "key = \"NN\""),
                "isn't a key crystal knows",
            ),
            (
                GOOD.replace("key = \"N\"", "key = \"j t\""),
                "crystal uses `j`",
            ),
            (
                GOOD.replace("key = \"N\"", "key = \"N t u\""),
                "more than two keys",
            ),
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
            (
                GOOD.replace("\"0.1\"", "\"soon\""),
                "isn't a version like 0.3.0",
            ),
            (
                GOOD.replace(r#"["macos", "linux"]"#, r#"["windows"]"#),
                "unknown variant `windows`",
            ),
            (
                GOOD.replace(r#"["macos", "linux"]"#, "[]"),
                "platforms is empty",
            ),
            (
                GOOD.replace(r#"["make"]"#, "[]"),
                "a build command has no command",
            ),
            (
                GOOD.replace("action = \"note\"", "action = \"open\""),
                "runs action open, which the plugin doesn't have",
            ),
            (GOOD.replace("[0-9]+$", "[0-9+$"), "link handler `^https"),
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
