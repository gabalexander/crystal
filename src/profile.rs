//! Profiles: named, saved ways of starting an agent. A profile says which
//! agent, set up how (its model, how hard it thinks, how it asks before
//! acting, more arguments), what it's asked before and after every task,
//! or instead of one, the standing instructions it keeps all session, where
//! it starts, and how: a session, a task or a background task. The
//! new-session panel offers them first.
//!
//! They live in the config file as `[[profile]]` tables. The TUI changes
//! them there through `toml_edit`, which keeps the rest of the file as the
//! user wrote it, comments and all.

use crate::catalog::{self, FirstPrompt, Instructions, Kind};
use crate::config;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

/// Whether profiles are offered: first in the new-session panel, in the
/// view `P` opens, and by `crystal profile`. Every part of crystal that
/// offers them asks here, so they can be switched off in one place. The
/// profiles in the config file are read and checked either way; switched
/// off, they're only not offered.
pub fn enabled(config: &config::Config) -> bool {
    crate::plugins::enabled(config, "profiles")
}

/// A saved way to start an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// What the panel calls it.
    pub name: String,
    /// A line on what it's for, shown in the panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The agent's program, one crystal knows: `claude`, `codex`, ….
    pub agent: String,
    /// The model, for an agent that takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// How hard it thinks: Claude Code's `--effort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// How it asks before acting: Claude Code's `--permission-mode`, or
    /// Codex's `-a`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// More arguments for the agent, as they'd be written after it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Text put in front of the task: what this profile always asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Text put after the task, like how to go about it or what to end
    /// with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postfix: Option<String>,
    /// Starts without asking for a task: its prompt and postfix alone are
    /// what it's asked.
    #[serde(default, skip_serializing_if = "is_false")]
    pub skip_task: bool,
    /// What the agent keeps in mind all session, on top of its own
    /// instructions: added to Claude Code's system prompt, or given to
    /// Codex as developer instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Where it starts, unless the panel is told otherwise. Left out, it
    /// starts wherever the panel is set to.
    #[serde(rename = "where", default, skip_serializing_if = "Option::is_none")]
    pub start_in: Option<StartIn>,
    /// How it's meant to start, which choosing it in the panel sets: left
    /// out, as the panel is set.
    #[serde(default, skip_serializing_if = "Launch::is_either")]
    pub launch: Launch,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// How a profile's agent is meant to start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Launch {
    /// As the new-session panel is set.
    #[default]
    Either,
    /// In a terminal, what it's asked only its first prompt: a session,
    /// not a task, with nothing to close.
    Session,
    /// In a terminal, as a task, which `crystal done` closes.
    Task,
    /// As a background task, `claude -p`, for Claude Code; another agent
    /// starts as a task in a terminal.
    Background,
}

impl Launch {
    /// Every one, in the order the profiles view goes through them.
    pub const ALL: [Launch; 4] = [
        Launch::Either,
        Launch::Session,
        Launch::Task,
        Launch::Background,
    ];

    fn is_either(&self) -> bool {
        *self == Launch::Either
    }

    /// Its word, as the config file has it.
    pub fn word(self) -> &'static str {
        match self {
            Launch::Either => "either",
            Launch::Session => "session",
            Launch::Task => "task",
            Launch::Background => "background",
        }
    }
}

/// Where a profile starts its agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StartIn {
    /// Where the selected session runs.
    Here,
    /// In a new worktree, on a branch with a made-up name.
    Worktree,
}

impl Profile {
    /// A profile for `agent` with nothing else set: what an agent chosen on
    /// its own in the panel amounts to.
    pub fn for_agent(agent: &str) -> Profile {
        Profile {
            name: String::new(),
            description: None,
            agent: agent.to_string(),
            model: None,
            effort: None,
            mode: None,
            args: Vec::new(),
            prompt: None,
            postfix: None,
            skip_task: false,
            instructions: None,
            start_in: None,
            launch: Launch::Either,
        }
    }

    /// The same, but for what it's asked before and after a task: its
    /// agent and its options.
    pub fn without_prompts(&self) -> Profile {
        Profile {
            prompt: None,
            postfix: None,
            ..self.clone()
        }
    }

    /// What it asks its agent on `task`: its prompt, the task and its
    /// postfix, those there are, a blank line apart.
    pub fn asked(&self, task: &str) -> String {
        let parts = [
            filled(&self.prompt),
            Some(task.trim()),
            filled(&self.postfix),
        ];
        let parts: Vec<&str> = parts
            .into_iter()
            .flatten()
            .filter(|p| !p.is_empty())
            .collect();
        parts.join("\n\n")
    }

    /// What it sets the agent's row of `kind` to, if anything.
    pub fn choice(&self, kind: Kind) -> &Option<String> {
        match kind {
            Kind::Model => &self.model,
            Kind::Effort => &self.effort,
            Kind::Mode => &self.mode,
        }
    }

    /// The same, to change it.
    pub fn choice_mut(&mut self, kind: Kind) -> &mut Option<String> {
        match kind {
            Kind::Model => &mut self.model,
            Kind::Effort => &mut self.effort,
            Kind::Mode => &mut self.mode,
        }
    }

    /// The command line that starts it on `task`: the agent, its options in
    /// the order of its rows, its instructions and arguments, then its
    /// prompt, the task and its postfix, the way the agent takes a first
    /// prompt.
    pub fn command(&self, task: &str) -> Vec<String> {
        let agent = catalog::find(&self.agent);
        let mut command = vec![self.agent.clone()];
        for setting in agent.map_or(&[][..], |agent| agent.settings) {
            if let Some(value) = self.choice(setting.kind) {
                command.push(setting.option.to_string());
                command.push(value.clone());
            }
        }
        if let Some(text) = filled(&self.instructions) {
            let instructions = agent.map_or(Instructions::None, |agent| agent.instructions);
            command.extend(instructions.args(text));
        }
        command.extend(self.args.iter().cloned());
        let prompt = self.asked(task);
        let first_prompt = agent.map_or(FirstPrompt::Argument, |agent| agent.first_prompt);
        first_prompt.add(&mut command, &prompt);
        command
    }

    /// A profile that can't be started as written is an error that says
    /// why: no name, an agent crystal doesn't know, or something its agent
    /// doesn't take.
    pub fn check(&self) -> Result<()> {
        let name = &self.name;
        if name.trim().is_empty() {
            bail!("a profile has no name");
        }
        self.check_settings(&format!("profile {name}"))
    }

    /// What [`Profile::check`] checks but its name, each error starting
    /// with `what`: for a profile a flow's step puts together, which may
    /// have none.
    pub fn check_settings(&self, what: &str) -> Result<()> {
        let Some(agent) = catalog::find(&self.agent) else {
            let known: Vec<&str> = catalog::AGENTS.iter().map(|a| a.program).collect();
            bail!(
                "{what}: crystal doesn't know the agent {}; it knows {}",
                self.agent,
                known.join(", ")
            );
        };
        for kind in [Kind::Model, Kind::Effort, Kind::Mode] {
            let Some(chosen) = self.choice(kind) else {
                continue;
            };
            if agent.setting(kind).is_none() {
                bail!("{what}: {} doesn't take {}", agent.name, kind.noun());
            }
            // Models are too many to know, and Codex's change as it ships
            // them; the rest are fixed.
            let values = agent.values(kind);
            if kind != Kind::Model && !values.contains(&chosen.as_str()) {
                bail!(
                    "{what}: {chosen} isn't {} of {}; it takes {}",
                    kind.noun(),
                    agent.name,
                    values.join(", ")
                );
            }
        }
        if filled(&self.instructions).is_some() && agent.instructions == Instructions::None {
            bail!("{what}: {} can't be given instructions", agent.name);
        }
        Ok(())
    }
}

/// The text, unless it's missing or only blanks.
fn filled(text: &Option<String>) -> Option<&str> {
    text.as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// Writes `profile` into the config file at `path`: in place of the profile
/// called `replacing`, keeping its place in the file, or after the others
/// when that's `None`. Nothing is written unless the file, with the
/// change, still makes sense.
pub fn save(path: &Path, replacing: Option<&str>, profile: &Profile) -> Result<()> {
    edit(path, |profiles| {
        match replacing {
            Some(old_name) => {
                let table = find(profiles, old_name)
                    .with_context(|| format!("there's no profile called {old_name}"))?;
                fill(table, profile);
            }
            None => {
                let mut table = Table::new();
                fill(&mut table, profile);
                profiles.push(table);
            }
        }
        Ok(())
    })
}

/// Takes the profile called `name` out of the config file at `path`.
pub fn delete(path: &Path, name: &str) -> Result<()> {
    edit(path, |profiles| {
        let index = profiles
            .iter()
            .position(|table| table_name(table) == Some(name))
            .with_context(|| format!("there's no profile called {name}"))?;
        profiles.remove(index);
        Ok(())
    })
}

/// Reads the config file at `path`, lets `change` change its profiles, and
/// writes it back, unless the result doesn't make sense as a config file.
/// The new file is written beside the old one and then moved over it, so
/// a crash halfway through can't leave half a file.
fn edit(path: &Path, change: impl FnOnce(&mut ArrayOfTables) -> Result<()>) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("couldn't read {}", path.display())),
    };
    let mut document: DocumentMut = text
        .parse()
        .with_context(|| format!("couldn't read {}", path.display()))?;
    let profiles = document
        .entry("profile")
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .context("`profile` in the config file isn't a list of [[profile]] tables")?;
    change(profiles)?;
    if profiles.is_empty() {
        document.remove("profile");
    }

    let new_text = document.to_string();
    config::from_text(&new_text)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let unfinished = path.with_extension("toml.saving");
    std::fs::write(&unfinished, &new_text)?;
    std::fs::rename(&unfinished, path)?;
    Ok(())
}

fn find<'a>(profiles: &'a mut ArrayOfTables, name: &str) -> Option<&'a mut Table> {
    profiles
        .iter_mut()
        .find(|table| table_name(table) == Some(name))
}

fn table_name(table: &Table) -> Option<&str> {
    table.get("name").and_then(Item::as_str)
}

/// Writes `profile` into `table`, one key at a time. A key whose value
/// doesn't change is left alone, with any comment beside it; one the
/// profile doesn't set is taken out.
fn fill(table: &mut Table, profile: &Profile) {
    set_text(table, "name", Some(&profile.name));
    set_text(table, "description", filled(&profile.description));
    set_text(table, "agent", Some(&profile.agent));
    set_text(table, "model", profile.model.as_deref());
    set_text(table, "effort", profile.effort.as_deref());
    set_text(table, "mode", profile.mode.as_deref());
    set_words(table, "args", &profile.args);
    set_text(table, "prompt", filled(&profile.prompt));
    set_text(table, "postfix", filled(&profile.postfix));
    set_flag(table, "skip_task", profile.skip_task);
    set_text(table, "instructions", filled(&profile.instructions));
    let start_in = profile.start_in.map(|start_in| match start_in {
        StartIn::Here => "here",
        StartIn::Worktree => "worktree",
    });
    set_text(table, "where", start_in);
    let launch = (profile.launch != Launch::Either).then(|| profile.launch.word());
    set_text(table, "launch", launch);
}

/// Sets `key` to true, or takes it out for false, the default.
fn set_flag(table: &mut Table, key: &str, on: bool) {
    if !on {
        table.remove(key);
    } else if table.get(key).and_then(Item::as_bool) != Some(true) {
        table.insert(key, value(true));
    }
}

fn set_text(table: &mut Table, key: &str, text: Option<&str>) {
    let Some(text) = text else {
        table.remove(key);
        return;
    };
    if table.get(key).and_then(Item::as_str) != Some(text) {
        table.insert(key, value(text));
    }
}

fn set_words(table: &mut Table, key: &str, words: &[String]) {
    if words.is_empty() {
        table.remove(key);
        return;
    }
    let current: Option<Vec<&str>> = table
        .get(key)
        .and_then(Item::as_array)
        .map(|array| array.iter().filter_map(|word| word.as_str()).collect());
    if current.as_deref() != Some(&words.iter().map(String::as_str).collect::<Vec<_>>()[..]) {
        let array: Array = words.iter().map(String::as_str).collect();
        table.insert(key, value(array));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review() -> Profile {
        Profile {
            name: "review".into(),
            description: Some("A second pair of eyes".into()),
            agent: "claude".into(),
            model: Some("opus".into()),
            effort: Some("high".into()),
            mode: Some("plan".into()),
            args: vec!["--verbose".into()],
            prompt: Some("Review the diff on this branch.".into()),
            postfix: None,
            skip_task: false,
            instructions: Some("Point out risks before style.".into()),
            start_in: Some(StartIn::Here),
            launch: Launch::Either,
        }
    }

    #[test]
    fn a_claude_profile_runs_with_its_options_instructions_and_prompt() {
        assert_eq!(
            review().command("Mind the tests."),
            [
                "claude",
                "--model",
                "opus",
                "--effort",
                "high",
                "--permission-mode",
                "plan",
                "--append-system-prompt",
                "Point out risks before style.",
                "--verbose",
                "--",
                "Review the diff on this branch.\n\nMind the tests.",
            ]
        );
    }

    #[test]
    fn a_codex_profile_gives_its_instructions_as_a_setting() {
        let profile = Profile {
            model: Some("gpt-6-luna".into()),
            mode: Some("on-request".into()),
            instructions: Some("Keep changes small.".into()),
            prompt: None,
            args: Vec::new(),
            ..Profile::for_agent("codex")
        };
        assert_eq!(
            profile.command("add a test"),
            [
                "codex",
                "-m",
                "gpt-6-luna",
                "-a",
                "on-request",
                "-c",
                "developer_instructions=\"Keep changes small.\"",
                "--",
                "add a test",
            ]
        );
    }

    #[test]
    fn a_prompt_alone_is_the_first_prompt_and_no_task_leaves_none() {
        let mut profile = review();
        assert_eq!(
            profile.command("").last().unwrap(),
            "Review the diff on this branch."
        );
        profile.prompt = None;
        assert_eq!(profile.command("").last().unwrap(), "--verbose");
    }

    #[test]
    fn a_postfix_goes_after_the_task_and_alone_with_the_prompt_without_one() {
        let profile = Profile {
            postfix: Some("End with what you tested.".into()),
            ..review()
        };
        assert_eq!(
            profile.command("Mind the tests.").last().unwrap(),
            "Review the diff on this branch.\n\nMind the tests.\n\nEnd with what you tested."
        );
        assert_eq!(
            profile.asked(""),
            "Review the diff on this branch.\n\nEnd with what you tested."
        );
        let options = profile.without_prompts().command("");
        assert_eq!(options.last().unwrap(), "--verbose");
    }

    #[test]
    fn launch_and_skip_task_are_written_only_when_set() {
        let (_dir, path) = file("");
        let committer = Profile {
            name: "committer".into(),
            prompt: Some("Commit the working tree.".into()),
            postfix: Some("Don't push.".into()),
            skip_task: true,
            launch: Launch::Session,
            ..Profile::for_agent("claude")
        };
        save(&path, None, &committer).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("skip_task = true\n"), "{text}");
        assert!(text.contains("launch = \"session\"\n"), "{text}");
        assert_eq!(
            config::from_text(&text).unwrap().profiles,
            std::slice::from_ref(&committer)
        );
        let either = Profile {
            skip_task: false,
            launch: Launch::Either,
            ..committer
        };
        save(&path, Some("committer"), &either).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("skip_task") && !text.contains("launch"),
            "{text}"
        );
        let err = config::from_text(
            "[[profile]]\nname = \"x\"\nagent = \"claude\"\nlaunch = \"remote\"\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("remote"), "{err:#}");
    }

    #[test]
    fn a_profile_that_cant_start_says_why() {
        let cases = [
            (Profile::for_agent("vim"), "doesn't know the agent vim"),
            (
                Profile {
                    model: Some("o3".into()),
                    ..Profile::for_agent("aider")
                },
                "doesn't take a model",
            ),
            (
                Profile {
                    mode: Some("yolo".into()),
                    ..Profile::for_agent("claude")
                },
                "isn't a mode of Claude Code",
            ),
            (
                Profile {
                    effort: Some("turbo".into()),
                    ..Profile::for_agent("claude")
                },
                "turbo isn't an effort level of Claude Code; it takes low, medium",
            ),
            (
                Profile {
                    effort: Some("high".into()),
                    ..Profile::for_agent("codex")
                },
                "Codex doesn't take an effort level",
            ),
            (
                Profile {
                    instructions: Some("Be brief.".into()),
                    ..Profile::for_agent("gemini")
                },
                "can't be given instructions",
            ),
        ];
        for (mut profile, expected) in cases {
            profile.name = "x".into();
            let err = profile.check().unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{err:#}");
        }
        assert!(Profile::for_agent("claude").check().is_err(), "no name");
    }

    fn file(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn saving_a_new_profile_keeps_the_rest_of_the_file_as_it_was() {
        let (_dir, path) = file("# my settings\nnotify = false # quiet, please\n");
        save(&path, None, &review()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# my settings\nnotify = false # quiet, please\n"));
        let config = config::from_text(&text).unwrap();
        assert_eq!(config.profiles, [review()]);
    }

    #[test]
    fn saving_over_a_profile_changes_only_what_changed() {
        let (_dir, path) = file(
            "[[profile]]\n# the reviewer\nname = \"review\"\nagent = \"claude\"\nmode = \"plan\" # careful\n",
        );
        let changed = Profile {
            model: Some("opus".into()),
            mode: Some("plan".into()),
            ..review()
        };
        save(&path, Some("review"), &changed).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# the reviewer\nname = \"review\""), "{text}");
        assert!(text.contains("mode = \"plan\" # careful"), "{text}");
        assert_eq!(config::from_text(&text).unwrap().profiles, [changed]);
    }

    #[test]
    fn a_change_that_doesnt_make_sense_is_never_written() {
        let original = "[[profile]]\nname = \"review\"\nagent = \"claude\"\n";
        let (_dir, path) = file(original);
        let broken = Profile {
            mode: Some("yolo".into()),
            ..review()
        };
        assert!(save(&path, Some("review"), &broken).is_err());
        let twin = Profile {
            name: "review".into(),
            ..Profile::for_agent("codex")
        };
        let err = save(&path, None, &twin).unwrap_err();
        assert!(format!("{err:#}").contains("two profiles are called review"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn deleting_the_last_profile_leaves_no_empty_list() {
        let (_dir, path) =
            file("notify = false\n\n[[profile]]\nname = \"review\"\nagent = \"claude\"\n");
        delete(&path, "review").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("profile"), "{text}");
        assert!(text.contains("notify = false"));
        assert!(delete(&path, "review").is_err());
    }

    #[test]
    fn a_config_file_not_there_yet_is_made() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crystal").join("config.toml");
        save(&path, None, &review()).unwrap();
        assert_eq!(
            config::from_text(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .profiles,
            [review()]
        );
    }
}
